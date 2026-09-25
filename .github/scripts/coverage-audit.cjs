'use strict';

const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const cp = require('node:child_process');

const NATIVE = ['x86_64-unknown-linux-gnu', 'x86_64-pc-windows-msvc', 'aarch64-apple-darwin'];
const TARGETS = {
  primary: NATIVE, msrv: NATIVE, tooling: NATIVE, branch: NATIVE,
  extended: ['x86_64-apple-darwin', 'aarch64-unknown-linux-gnu'],
};
const METRICS = ['lines', 'regions', 'functions', 'instantiations'];
const MAX_BYTES = 64 * 1024 * 1024;
function requireValue(value, message) { if (!value) throw new Error(message); }
function digest(bytes) { return crypto.createHash('sha256').update(bytes).digest('hex'); }
function sourcePath(value) {
  requireValue(typeof value === 'string', 'Source path must be text');
  const normalized = value.replaceAll('\\', '/');
  requireValue(/^[A-Za-z0-9_./-]+$/.test(normalized)
    && !normalized.startsWith('/') && !normalized.split('/').some(p => p === '.' || p === '..' || p === ''),
  'Unsafe source path: ' + value);
  return normalized;
}
function readRegular(root, relative) {
  const name = sourcePath(relative);
  const base = fs.realpathSync(root);
  let file = base;
  for (const part of name.split('/')) {
    file = path.join(file, part);
    requireValue(!fs.lstatSync(file).isSymbolicLink(), 'Linked evidence path: ' + name);
  }
  requireValue(fs.realpathSync(file) === file, 'Redirected evidence path: ' + name);
  const fd = fs.openSync(file, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW || 0));
  try {
    const before = fs.fstatSync(fd);
    requireValue(before.isFile() && before.size > 0 && before.size <= MAX_BYTES,
      'Empty, oversized, or non-file evidence: ' + name);
    const bytes = fs.readFileSync(fd);
    const after = fs.fstatSync(fd);
    const current = fs.lstatSync(file);
    requireValue(!current.isSymbolicLink() && fs.realpathSync(file) === file
      && current.dev === before.dev && current.ino === before.ino
      && current.size === before.size && current.mtimeMs === before.mtimeMs,
    'Evidence path replaced while reading: ' + name);
    requireValue(before.dev === after.dev && before.ino === after.ino
      && before.size === bytes.length && after.size === before.size && after.mtimeMs === before.mtimeMs,
    'Evidence changed while reading: ' + name);
    return bytes;
  } finally { fs.closeSync(fd); }
}
function counter(value, label) {
  requireValue(value && Number.isSafeInteger(value.count) && Number.isSafeInteger(value.covered)
    && value.count > 0 && value.covered >= 0 && value.covered <= value.count,
  'Invalid or empty counter: ' + label);
  return {count: value.count, covered: value.covered};
}
function ratchet(value, baseline, label) {
  const current = counter(value, label);
  const old = counter(baseline, label + ' baseline');
  requireValue(current.count - current.covered <= old.count - old.covered
    && BigInt(current.covered) * BigInt(old.count) >= BigInt(old.covered) * BigInt(current.count),
  'Coverage regression: ' + label);
}
function gapLocations(data) {
  requireValue(Array.isArray(data.functions) && data.functions.length > 0, 'Missing LLVM function entries');
  const groups = new Map();
  for (const fn of data.functions) {
    requireValue(typeof fn.name === 'string' && fn.name.length > 0
      && Array.isArray(fn.filenames) && fn.filenames.length > 0
      && fn.filenames.every(file => typeof file === 'string' && file.length > 0)
      && Number.isSafeInteger(fn.count) && fn.count >= 0
      && Array.isArray(fn.regions) && fn.regions.length > 0, 'Invalid LLVM function identity');
    const regions = [];
    for (const region of fn.regions) {
      requireValue(Array.isArray(region) && region.length === 8
        && region.every(value => Number.isSafeInteger(value) && value >= 0)
        && region.slice(0, 4).every(value => value > 0)
        && region[5] < fn.filenames.length
        && (region[2] > region[0] || (region[2] === region[0] && region[3] >= region[1])),
      'Invalid LLVM region identity');
      if (region[4] === 0 && region[7] === 0) regions.push([...region.slice(0, 4), region[5]]);
    }
    const key = JSON.stringify([fn.name, fn.filenames]);
    if (!groups.has(key)) groups.set(key, {symbol: fn.name, files: fn.filenames, missed_entries: 0, regions: []});
    const group = groups.get(key);
    if (fn.count === 0) group.missed_entries++;
    group.regions.push(...regions);
  }
  return [...groups.entries()].sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)
    .map(([, group]) => ({...group, regions: group.regions.sort((a, b) => {
      for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return a[i] - b[i];
      return 0;
    })})).filter(group => group.missed_entries > 0 || group.regions.length > 0);
}
function gapBag(groups) {
  requireValue(Array.isArray(groups), 'Missing reviewed gap locations');
  const bag = new Map();
  for (const group of groups) {
    requireValue(typeof group.symbol === 'string' && Array.isArray(group.files)
      && group.files.every(file => typeof file === 'string')
      && Number.isSafeInteger(group.missed_entries) && group.missed_entries >= 0
      && Array.isArray(group.regions), 'Invalid reviewed gap locations');
    const add = (metric, coordinate, count) => {
      const key = JSON.stringify([metric, group.symbol, group.files, coordinate]);
      bag.set(key, (bag.get(key) || 0) + count);
    };
    if (group.missed_entries) add('json-entry', [], group.missed_entries);
    for (const region of group.regions) {
      requireValue(Array.isArray(region) && region.length === 5
        && region.every(value => Number.isSafeInteger(value) && value >= 0), 'Invalid reviewed region');
      add('instance-code-region', region, 1);
    }
  }
  return bag;
}
function ratchetGaps(current, baseline, target) {
  const old = gapBag(baseline);
  for (const [key, count] of gapBag(current)) requireValue(count <= (old.get(key) || 0),
    'New or increased missed location requires review: ' + target + '/' + key);
}
function addLine(map, file, line, hits) {
  if (!map.has(file)) map.set(file, new Map());
  const lines = map.get(file);
  lines.set(line, (lines.get(line) || 0n) + hits);
}
function lineSummary(map) {
  let count = 0, covered = 0;
  for (const lines of map.values()) for (const hits of lines.values()) {
    count++; if (hits > 0n) covered++;
  }
  requireValue(count > 0, 'No measured source lines');
  return {count, covered};
}
function parseLcov(text) {
  const lines = new Map(), summaries = [];
  let record = null;
  for (const row of text.split(/\r?\n/)) {
    if (row.startsWith('SF:')) {
      requireValue(record === null, 'Nested LCOV record');
      const file = sourcePath(row.slice(3));
      requireValue(!lines.has(file), 'Duplicate LCOV source record');
      record = {file, lines: new Map(), lf: null, lh: null};
    } else if (row.startsWith('DA:')) {
      const match = /^DA:(\d+),(\d{1,20})(?:,[a-fA-F0-9]+)?$/.exec(row);
      requireValue(record && match, 'Invalid LCOV line record');
      const line = Number(match[1]);
      requireValue(Number.isSafeInteger(line) && line > 0 && !record.lines.has(line),
        'Invalid or duplicate LCOV coordinate');
      record.lines.set(line, BigInt(match[2]));
    } else if (row.startsWith('LF:') || row.startsWith('LH:')) {
      requireValue(record && /^(LF|LH):\d+$/.test(row), 'Invalid LCOV summary');
      const key = row.startsWith('LF:') ? 'lf' : 'lh';
      requireValue(record[key] === null, 'Duplicate LCOV summary');
      record[key] = Number(row.slice(3));
    } else if (row.startsWith('BRDA:')) {
      throw new Error('Branch-bearing LCOV must remain separate from the line-only gate');
    } else if (row === 'end_of_record') {
      requireValue(record && record.lines.size > 0 && Number.isSafeInteger(record.lf)
        && Number.isSafeInteger(record.lh) && record.lh >= 0 && record.lf >= record.lh,
      'Incomplete LCOV record');
      const covered = [...record.lines.values()].filter(hits => hits > 0n).length;
      if (record.lf !== record.lines.size || record.lh !== covered) {
        summaries.push({file: record.file, reportedLf: record.lf, reportedLh: record.lh,
          uniqueDaLines: record.lines.size, coveredDaLines: covered});
      }
      lines.set(record.file, record.lines);
      record = null;
    } else if (row.startsWith('SF') || row.startsWith('DA')) {
      throw new Error('Malformed LCOV record');
    }
  }
  requireValue(record === null && lines.size > 0, 'Incomplete or empty LCOV');
  return {lines, summaries, totals: lineSummary(lines)};
}

function parseBranchLcov(text) {
  const stripped = [], branches = new Map();
  let record = null;
  for (const row of text.split(/\r?\n/)) {
    if (row.startsWith('SF:')) {
      requireValue(!record, 'Nested branch LCOV record');
      record = {file: sourcePath(row.slice(3)), rows: new Map(), found: null, hit: null};
      requireValue(!branches.has(record.file), 'Duplicate branch LCOV source');
    }
    if (row.startsWith('BRDA:')) {
      const match = /^BRDA:([1-9]\d*),(\d+),(\d+),(-|\d+)$/.exec(row);
      requireValue(record && match, 'Invalid LCOV branch record');
      const coordinate = match.slice(1, 4).map(Number);
      requireValue(coordinate.every(Number.isSafeInteger), 'Invalid LCOV branch coordinate');
      const key = coordinate.join(':');
      requireValue(!record.rows.has(key), 'Duplicate LCOV branch coordinate');
      const hits = match[4] === '-' ? 0n : BigInt(match[4]);
      record.rows.set(key, hits > 0n);
      continue;
    }
    if (row.startsWith('BRF:') || row.startsWith('BRH:')) {
      requireValue(record && /^(BRF|BRH):\d+$/.test(row), 'Invalid LCOV branch summary');
      const field = row.startsWith('BRF:') ? 'found' : 'hit';
      requireValue(record[field] === null, 'Duplicate LCOV branch summary');
      record[field] = Number(row.slice(4));
      continue;
    }
    if (row === 'end_of_record') {
      requireValue(record && Number.isSafeInteger(record.found) && Number.isSafeInteger(record.hit),
        'Missing LCOV branch summary');
      const covered = [...record.rows.values()].filter(Boolean).length;
      requireValue(record.found === record.rows.size && record.hit === covered, 'LCOV branch summary mismatch');
      branches.set(record.file, {count: record.found, covered});
      record = null;
    }
    stripped.push(row);
  }
  requireValue(!record && branches.size > 0, 'Incomplete branch LCOV');
  return {...parseLcov(stripped.join('\n')), branches};
}
function validateBranches(json, parsed, target, policy) {
  const baseline = policy.targets?.[target];
  requireValue(policy.schema === 1 && baseline && json.type === 'llvm.coverage.json.export'
    && json.version === policy.export_version && json.data?.length === 1, 'Invalid branch policy/export identity');
  const data = json.data[0], total = counter(data.totals?.branches, 'branches');
  requireValue(data.totals.branches.notcovered === total.count - total.covered
    && total.covered === total.count, 'Measured branch coverage is below 100%');
  requireValue(Array.isArray(data.files) && data.files.length > 0, 'Missing branch source records');
  const seen = new Set(), locations = [];
  let count = 0, covered = 0;
  for (const file of data.files) {
    const name = sourcePath(file.filename), conditions = new Map();
    requireValue(!seen.has(name) && Array.isArray(file.branches), 'Missing or duplicate branch source');
    seen.add(name);
    for (const row of file.branches) {
      requireValue(Array.isArray(row) && row.length === 9
        && row.every(v => Number.isSafeInteger(v) && v >= 0)
        && row.slice(0, 4).every(v => v > 0) && row[8] === 4
        && (row[2] > row[0] || (row[2] === row[0] && row[3] >= row[1])), 'Invalid LLVM branch identity');
      const key = JSON.stringify(row.slice(0, 4));
      if (!conditions.has(key)) conditions.set(key, {range: row.slice(0, 4), yes: false, no: false});
      const condition = conditions.get(key);
      condition.yes ||= row[4] > 0;
      condition.no ||= row[5] > 0;
    }
    const outcomes = conditions.size * 2;
    const hits = [...conditions.values()].reduce((sum, value) => sum + Number(value.yes) + Number(value.no), 0);
    const summary = file.summary?.branches, lcov = parsed.branches?.get(name);
    requireValue(summary && summary.count === outcomes && summary.covered === hits
      && summary.notcovered === outcomes - hits && lcov?.count === outcomes && lcov.covered === hits,
    'LLVM/LCOV branch outcomes differ: ' + name);
    requireValue(hits === outcomes, 'Uncovered physical branch outcome: ' + name);
    for (const condition of conditions.values()) locations.push(JSON.stringify([name, ...condition.range]));
    count += outcomes; covered += hits;
  }
  requireValue(count === total.count && covered === total.covered, 'Branch aggregate/physical outcomes differ');
  sameList([...seen], [...parsed.lines.keys()], 'Branch LCOV/JSON source sets differ');
  sameList([...seen], baseline.reported_files, 'Branch source inventory requires review');
  sameList(locations, baseline.locations.map(location => JSON.stringify(location)), 'Branch location inventory requires review');
  requireValue(total.count === baseline.outcomes, 'Branch denominator requires review');
  return {count, covered};
}

function prefixFor(profile, target) {
  requireValue(TARGETS[profile]?.includes(target), 'Unexpected profile/target');
  return 'coverage-' + (profile === 'primary' ? '' : profile + '-') + target;
}
function filesFor(profile, target) {
  const prefix = prefixFor(profile, target);
  const files = [prefix + '.lcov', prefix + '.json', prefix + '.txt',
    prefix + '-provenance.txt', prefix + '-source.sha256'];
  if (profile === 'primary' || profile === 'extended') files.push(prefix + '-codecov.json');
  if (profile === 'primary' || profile === 'msrv') {
    const family = profile === 'primary' ? 'coverage-' : 'coverage-msrv-';
    for (const kind of ['unit', 'integration']) {
      for (const ext of ['json', 'txt']) files.push(family + kind + '-' + target + '.' + ext);
    }
    files.push(family + 'diagnostics-' + target + '.json');
  }
  return files.sort();
}
function identity(root, env) {
  const git = (...args) => cp.execFileSync('git', args, {cwd: root, encoding: 'utf8', maxBuffer: MAX_BYTES}).trim();
  requireValue(/^[a-f0-9]{40}$/.test(env.GITHUB_SHA || '')
    && /^[1-9]\d*$/.test(env.GITHUB_RUN_ID || '') && /^[1-9]\d*$/.test(env.GITHUB_RUN_ATTEMPT || ''),
  'Expected exact CI revision, run, and attempt');
  requireValue(git('rev-parse', 'HEAD') === env.GITHUB_SHA, 'Checkout revision differs from CI');
  git('diff', '--exit-code', 'HEAD', '--');
  const sources = git('ls-files', '-z', '--', 'src', 'tools/fs2-dev/src').split('\0').filter(p => p.endsWith('.rs'));
  return {sha: env.GITHUB_SHA, tree: git('rev-parse', 'HEAD^{tree}'),
    run: env.GITHUB_RUN_ID, attempt: env.GITHUB_RUN_ATTEMPT, sources};
}
function checkProvenance(text, expected, profile, target, policy) {
  const toolchain = profile === 'msrv' ? '1.88.0' : policy.toolchain;
  const fields = text.split(/\r?\n/);
  const values = {
    requested_sha: expected.sha, checked_out_sha: expected.sha, tree: expected.tree,
    run_id: expected.run, run_attempt: expected.attempt, toolchain, target, CARGO_INCREMENTAL: '0',
  };
  for (const [key, value] of Object.entries(values)) {
    requireValue(fields.filter(row => row.startsWith(key + '=')).length === 1
      && fields.includes(key + '=' + value), 'Coverage provenance mismatch: ' + key);
  }
  requireValue(fields.includes('host: ' + target) && fields.includes('release: ' + toolchain)
    && fields.includes('cargo-llvm-cov ' + policy.llvm_cov), 'Compiler or exporter identity mismatch');
  if (profile === 'branch') {
    requireValue(fields.includes('commit-hash: ' + policy.compiler_commit)
      && fields.includes('LLVM version: ' + policy.llvm_version), 'Branch compiler identity mismatch');
    for (const key of ['runner_label', 'runner_image_os', 'runner_image_version', 'node_version']) {
      const records = fields.filter(row => row.startsWith(key + '='));
      requireValue(records.length === 1 && /^[A-Za-z0-9_.-]{1,100}$/.test(records[0].slice(key.length + 1))
        && records[0] !== key + '=unset', 'Missing runner/runtime identity: ' + key);
    }
    requireValue(fields.includes('runner_label=' + policy.targets[target].runner_label), 'Branch runner label mismatch');
  }
}
function seal(root, profile, target, expected) {
  const files = Object.fromEntries(filesFor(profile, target).map(name => [name, digest(readRegular(root, name))]));
  const receipt = {schema: 1, profile, target, sha: expected.sha, tree: expected.tree,
    run: expected.run, attempt: expected.attempt, files};
  fs.writeFileSync(path.join(root, prefixFor(profile, target) + '-receipt.json'),
    JSON.stringify(receipt, null, 2) + '\n', {flag: 'wx', mode: 0o600});
}
function sameList(actual, expected, message) {
  requireValue(JSON.stringify([...actual].sort()) === JSON.stringify([...expected].sort()), message);
}

function readEvidence(directory, profile, target, expected, policy) {
  const prefix = prefixFor(profile, target), artifact = path.join(directory, prefix);
  requireValue(!fs.lstatSync(artifact).isSymbolicLink(), 'Linked artifact directory');
  const receiptBytes = readRegular(artifact, prefix + '-receipt.json');
  const receipt = JSON.parse(receiptBytes);
  requireValue(receipt.schema === 1 && receipt.profile === profile && receipt.target === target,
    'Artifact receipt identity mismatch');
  for (const key of ['sha', 'tree', 'run']) requireValue(receipt[key] === expected[key], 'Stale/mixed artifact: ' + key);
  requireValue(receipt.attempt === expected.attempt,
    'Stale/mixed artifact: attempt. Re-run all jobs; partial reruns cannot reuse earlier producer attempts.');
  sameList(Object.keys(receipt.files || {}), filesFor(profile, target), 'Incomplete artifact receipt');
  const evidence = new Map();
  for (const name of filesFor(profile, target)) {
    const bytes = readRegular(artifact, name);
    requireValue(receipt.files[name] === digest(bytes), 'Artifact digest mismatch: ' + name);
    evidence.set(name, bytes.toString('utf8'));
  }
  checkProvenance(evidence.get(prefix + '-provenance.txt'), expected, profile, target, policy);
  return {prefix, evidence, receipt: digest(receiptBytes)};
}

function audit(root, directory, profile, expected, policy) {
  requireValue(policy.schema === 1 && TARGETS[profile], 'Unsupported coverage policy/profile');
  const inventory = profile === 'tooling' ? policy.tooling_inventory : policy.source_inventory;
  const sourceRoot = profile === 'tooling' ? 'tools/fs2-dev/src/' : 'src/';
  sameList(expected.sources.filter(p => p.startsWith(sourceRoot)), inventory, 'Source inventory requires review');
  const allowed = new Set(inventory), merged = new Map(), sources = new Map(), platforms = [];
  const failures = [];
  for (const target of TARGETS[profile]) {
    try {
    const {prefix, evidence, receipt} = readEvidence(directory, profile, target, expected, policy);
    const manifest = new Map();
    for (const row of evidence.get(prefix + '-source.sha256').trimEnd().split(/\r?\n/)) {
      const match = /^([a-f0-9]{64}) [ *](.+)$/.exec(row);
      requireValue(match, 'Invalid source manifest');
      const file = sourcePath(match[2]);
      requireValue(!manifest.has(file), 'Duplicate source manifest path');
      manifest.set(file, match[1]);
    }
    const parsed = (profile === 'branch' ? parseBranchLcov : parseLcov)(evidence.get(prefix + '.lcov'));
    const json = JSON.parse(evidence.get(prefix + '.json'));
    requireValue(json.type === 'llvm.coverage.json.export' && json.data?.length === 1, 'Invalid LLVM JSON envelope');
    const data = json.data[0], totals = {};
    const gaps = gapLocations(data);
    for (const metric of METRICS) totals[metric] = counter(data.totals?.[metric], metric);
    sameList(data.files.map(file => sourcePath(file.filename)), [...parsed.lines.keys()], 'LCOV/JSON source sets differ');
    for (const [file, lines] of parsed.lines) {
      requireValue(allowed.has(file), 'Reported source outside the reviewed scope: ' + file);
      if (!sources.has(file)) {
        const bytes = readRegular(root, file);
        const lf = bytes.toString('utf8').replaceAll('\r\n', '\n');
        sources.set(file, {hashes: new Set([digest(bytes), digest(lf), digest(lf.replaceAll('\n', '\r\n'))]),
          lines: lf.split('\n').length});
      }
      const source = sources.get(file);
      requireValue(source.hashes.has(manifest.get(file)), 'Source digest mismatch: ' + file);
      for (const [line, hits] of lines) {
        requireValue(line <= source.lines, 'Coverage coordinate outside source: ' + file);
        addLine(merged, file, line, hits);
      }
    }
    if (evidence.has(prefix + '-codecov.json')) {
      const pilot = JSON.parse(evidence.get(prefix + '-codecov.json'));
      requireValue(pilot.coverage && typeof pilot.coverage === 'object' && !Array.isArray(pilot.coverage)
        && Object.keys(pilot.coverage).length > 0, 'Empty region-aware pilot');
      for (const file of Object.keys(pilot.coverage)) requireValue(allowed.has(sourcePath(file)), 'Pilot source outside scope');
    }
    const branches = profile === 'branch' ? validateBranches(json, parsed, target, policy) : null;
    let diagnostics = null;
    if (profile === 'primary' || profile === 'msrv') {
      const family = profile === 'primary' ? 'coverage-' : 'coverage-msrv-';
      diagnostics = JSON.parse(evidence.get(family + 'diagnostics-' + target + '.json'));
      requireValue(diagnostics.schema_version === 5 && diagnostics.target === target, 'Invalid diagnostic identity');
      requireValue(JSON.stringify(diagnostics.instantiation_policy) === JSON.stringify({
        unit_profile: 'required-complete', combined_profile: 'compiler-sensitive-diagnostic',
        integration_profile: 'reviewed-unit-owned-residuals',
      }), 'Invalid instantiation policy');
      requireValue(Array.isArray(diagnostics.profiles), 'Missing profile diagnostics');
      const profiles = new Map();
      for (const diagnostic of diagnostics.profiles) {
        requireValue(typeof diagnostic.profile === 'string' && !profiles.has(diagnostic.profile),
          'Missing or duplicate profile diagnostic');
        profiles.set(diagnostic.profile, diagnostic);
      }
      sameList([...profiles.keys()].sort(), ['combined', 'integration', 'unit'], 'Profile diagnostics differ');
      const combined = profiles.get('combined'), unit = profiles.get('unit'), integration = profiles.get('integration');
      const combinedInstantiations = counter(combined.llvm_instantiations, 'Raw combined instantiations');
      sameList([JSON.stringify(combinedInstantiations)], [JSON.stringify(totals.instantiations)],
        'Diagnostic/LLVM instantiations differ');
      const unitInstantiations = counter(unit.llvm_instantiations, 'Unit instantiations');
      requireValue(unitInstantiations.covered === unitInstantiations.count,
        'Unit instantiations are below 100%');
      requireValue(unit.asymmetric_definition_groups === 0, 'Unit profile contains compiler-asymmetric definitions');
      for (const [name, diagnostic] of profiles) {
        const definitions = counter(diagnostic.source_definitions, name + ' source definitions');
        requireValue(Array.isArray(diagnostic.definitions)
          && diagnostic.definitions.length === definitions.count
          && diagnostic.definitions.filter(d => d.covered_entries > 0).length === definitions.covered,
        'Source-definition diagnostic mismatch');
        requireValue(diagnostic.definitions.every(d => d.ownership !== 'unowned'),
          'Unowned source definition in ' + name + ' profile');
        const union = diagnostic.source_location_execution_union;
        const locations = counter(union?.locations, name + ' source-location union');
        requireValue(union.policy_enforced === true && Array.isArray(union.records),
          'Invalid source-location policy diagnostic');
        if (name === 'combined' || name === 'unit') {
          requireValue(locations.covered === locations.count, 'Incomplete ' + name + ' source-location union');
        } else {
          requireValue(union.records.filter(record => !record.executed).every(record =>
            record.all_uncovered_topologies_unit_owned === true && record.reviewed_integration_gap),
          'Unreviewed or unowned integration source-location gap');
        }
      }
      const current = combined;
      const entries = {count: data.functions.length, covered: data.functions.filter(f => f.count > 0).length};
      sameList([JSON.stringify(counter(current.json_entries, 'JSON entries'))],
        [JSON.stringify(counter(entries, 'LLVM function entries'))], 'Diagnostic/LLVM function entries differ');
      const definitions = counter(current.source_definitions, 'Source definitions');
      if (profile === 'primary') {
        const baseline = policy.targets[target];
        requireValue(baseline, 'Missing primary baseline');
        requireValue(baseline.gap_provenance?.toolchain === policy.toolchain
          && baseline.gap_provenance?.llvm_cov === policy.llvm_cov
          && baseline.gap_provenance?.export_version === json.version,
        'Reviewed gap compiler/export identity mismatch');
        ratchetGaps(gaps, baseline.reviewed_gaps, target);
        sameList([...parsed.lines.keys()], baseline.reported_files, 'Reported file inventory requires review');
        requireValue(parsed.totals.covered === parsed.totals.count, 'Primary line coverage is below 100%');
        for (const metric of METRICS) ratchet(totals[metric], baseline.llvm_totals[metric], target + '/' + metric);
        ratchet(entries, baseline.json_entries, target + '/JSON entries');
        const instantiationBaseline = baseline.instantiation_profiles;
        requireValue(instantiationBaseline, 'Missing instantiation-profile baseline');
        const oldUnit = counter(instantiationBaseline.unit, target + '/unit instantiations baseline');
        requireValue(unitInstantiations.count >= oldUnit.count,
          'Unit instantiation denominator requires review: ' + target);
        ratchet(unitInstantiations, oldUnit, target + '/unit instantiations');
        for (const [diagnostic, maximum, label] of [
          [combined, instantiationBaseline.maximum_combined_asymmetric_definition_groups, 'combined'],
          [integration, instantiationBaseline.maximum_integration_asymmetric_definition_groups, 'integration'],
        ]) {
          requireValue(Number.isSafeInteger(maximum) && maximum >= 0
            && Number.isSafeInteger(diagnostic.asymmetric_definition_groups)
            && diagnostic.asymmetric_definition_groups >= 0
            && diagnostic.asymmetric_definition_groups <= maximum,
          'Compiler-asymmetric definition growth requires review: ' + target + '/' + label);
        }
        for (const [value, old, label] of [
          [definitions, baseline.source_definitions, 'source definitions'],
          [diagnostics.intended_integration_definitions, baseline.intended_integration_definitions, 'intended integration definitions'],
        ]) {
          const checked = counter(value, label);
          requireValue(checked.covered === checked.count && checked.count >= old.count, 'Incomplete ' + label);
        }
      }
      diagnostics = {
        instantiation_policy: diagnostics.instantiation_policy,
        intended_integration_definitions: diagnostics.intended_integration_definitions,
        profiles: diagnostics.profiles.map(p => ({profile: p.profile, llvm_instantiations: p.llvm_instantiations,
          json_entries: p.json_entries, source_definitions: p.source_definitions,
          asymmetric_definition_groups: p.asymmetric_definition_groups,
          source_location_execution_union: {
            policy_enforced: p.source_location_execution_union.policy_enforced,
            locations: p.source_location_execution_union.locations,
            reviewed_unexecuted_locations: p.source_location_execution_union.records
              .filter(record => !record.executed).length,
          }})),
      };
    }
    platforms.push({target, lines: parsed.totals, llvm: totals, diagnostics, missed_locations: gaps,
      lcov_summary_differences: parsed.summaries, receipt, ...(branches ? {branches} : {})});
    } catch (error) {
      failures.push({target, message: error.message});
    }
  }
  if (failures.length) {
    const error = new Error(failures.map(f => f.target + ': ' + f.message).join('\n'));
    error.failures = failures;
    error.completedTargets = platforms.map(p => p.target);
    throw error;
  }
  return {schema: 1, status: 'complete', sha: expected.sha, tree: expected.tree, run: expected.run, attempt: expected.attempt,
    profile, toolchain: profile === 'msrv' ? '1.88.0' : policy.toolchain,
    policy_baseline: policy.baseline_sha, platforms, merged_unique_lines: lineSummary(merged),
    source_inventory: inventory, sources_without_line_records: inventory.filter(file => !merged.has(file)),
    scope: profile === 'tooling' ? 'fs2-dev Rust sources; not all repository scripts' : 'fs2-turbo library coverage reports',
    line_gate: profile === 'primary' ? '100% required per native target' : 'Informational coverage; tests and evidence integrity required',
    llvm_note: 'Raw LLVM metrics and compiler-emitted entries are not interchangeable with physical source-line coverage'};
}
function formatCount(value) { return value.covered + '/' + value.count; }
function render(report) {
  if (report.profile === 'branch') return [
    '# Coverage evidence: branch', '',
    'SHA: ' + report.sha + '; Rust ' + report.toolchain + '; run ' + report.run + ', attempt ' + report.attempt + '.', '',
    '| Target | Measured branch outcomes |', '| --- | ---: |',
    ...report.platforms.map(p => '| ' + p.target + ' | ' + formatCount(p.branches) + ' |'), '',
    'Every native target independently requires 100% of its reviewed measured branch outcomes.',
    'Nightly instrumentation does not measure every Rust construct, MC/DC, or complete instantiation coverage.',
    'These reports remain separate from stable line gates and Codecov publication.', '',
  ].join('\n');
  const profileAware = report.profile === 'primary' || report.profile === 'msrv';
  const header = profileAware
    ? '| Target | Unique lines | LLVM aggregate lines | Regions | Functions | Raw combined instantiations | Unit instantiations (gate) |'
    : '| Target | Unique lines | LLVM aggregate lines | Regions | Functions | Instantiations |';
  const separator = profileAware
    ? '| --- | ---: | ---: | ---: | ---: | ---: | ---: |'
    : '| --- | ---: | ---: | ---: | ---: | ---: |';
  const rows = report.platforms.map(p => {
    const values = [p.lines, p.llvm.lines, p.llvm.regions, p.llvm.functions, p.llvm.instantiations];
    if (profileAware) values.push(p.diagnostics.profiles.find(profile => profile.profile === 'unit').llvm_instantiations);
    return '| ' + p.target + ' | ' + values.map(formatCount).join(' | ') + ' |';
  });
  return [
    '# Coverage evidence: ' + report.profile, '',
    'SHA: ' + report.sha + '; Rust ' + report.toolchain + '; run ' + report.run + ', attempt ' + report.attempt + '.',
    '', 'Scope: ' + report.scope + '.', '',
    header, separator, ...rows,
    '', 'Merged unique lines: ' + formatCount(report.merged_unique_lines) + '. ' + report.line_gate + '.',
    '', report.llvm_note + '.',
    '', 'Files without line records are listed in JSON, not silently classified as covered.',
    'Unit-profile instantiations are a strict 100% gate; raw combined instantiations remain compiler-sensitive diagnostics.',
    'Integration-only source-location gaps must be explicitly reviewed and covered by the unit profile.',
    'The region-aware Codecov export is an artifact-only pilot, not the published LCOV score.', '',
  ].join('\n');
}
function escapeHtml(text) {
  return String(text).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;').replaceAll("'", '&#39;');
}
function renderFailure(report) {
  return [
    '# Coverage evidence rejected', '',
    'No coverage result from this collection is eligible for publication.', '',
    '<pre>' + escapeHtml(JSON.stringify({
      profile: report.profile, requested_sha: report.requested_sha,
      run: report.run, attempt: report.attempt, upstream_result: report.upstream_result,
      failures: report.failures, completed_targets: report.completed_targets,
    }, null, 2)) + '</pre>', '',
    'Recovery: fix the reported cause, then choose **Re-run all jobs**. Partial reruns',
    'cannot reuse artifacts from an earlier attempt. Do not delete receipts or relax',
    'identity checks to make an incomplete collection pass.', '',
  ].join('\n');
}
function writeSummary(report, env) {
  requireValue(env.RUNNER_TEMP && env.GITHUB_STEP_SUMMARY, 'CI output paths required');
  const label = Object.hasOwn(TARGETS, report.profile) ? report.profile : 'invalid';
  const output = fs.mkdtempSync(path.join(fs.realpathSync(env.RUNNER_TEMP), 'fs2-coverage-' + label + '-'));
  const markdown = report.status === 'complete' ? render(report) : renderFailure(report);
  fs.writeFileSync(path.join(output, 'summary.json'), JSON.stringify(report, null, 2) + '\n', {flag: 'wx', mode: 0o600});
  fs.writeFileSync(path.join(output, 'summary.md'), markdown, {flag: 'wx', mode: 0o600});
  fs.appendFileSync(env.GITHUB_STEP_SUMMARY, markdown);
  if (env.GITHUB_OUTPUT) fs.appendFileSync(env.GITHUB_OUTPUT, 'report_directory=' + output + '\n');
  return output;
}
function main(args = process.argv.slice(2), root = process.cwd(), env = process.env, identify = identity) {
  const [command, profile, argument] = args;
  try {
    requireValue(args.length === 3 && (command === 'seal' || command === 'collect') && Object.hasOwn(TARGETS, profile),
      'Usage: coverage-audit.cjs seal PROFILE TARGET | collect PROFILE ARTIFACT_DIRECTORY');
    const expected = identify(root, env);
    const policy = JSON.parse(readRegular(root, profile === 'branch'
      ? '.github/coverage-branch-policy.json' : '.github/coverage-policy.json'));
    if (command === 'seal') {
      seal(root, profile, argument, expected);
    } else {
      const report = audit(root, path.resolve(root, argument), profile, expected, policy);
      requireValue(env.COVERAGE_JOB_RESULT === 'success', 'Native coverage jobs did not all succeed: ' + env.COVERAGE_JOB_RESULT);
      const output = writeSummary(report, env);
      console.log(JSON.stringify({profile, sha: expected.sha, lines: report.merged_unique_lines, output}));
    }
    return 0;
  } catch (error) {
    if (command === 'collect') {
      const report = {schema: 1, status: 'rejected', profile,
        requested_sha: env.GITHUB_SHA, run: env.GITHUB_RUN_ID, attempt: env.GITHUB_RUN_ATTEMPT,
        upstream_result: env.COVERAGE_JOB_RESULT,
        completed_targets: error.completedTargets || [],
        failures: error.failures || [{message: error.message}]};
      try { writeSummary(report, env); }
      catch (outputError) { console.error('Could not preserve failure summary: ' + outputError.message); }
    }
    console.error(error.message);
    return 1;
  }
}
module.exports = {TARGETS, METRICS, sourcePath, readRegular, counter, ratchet, parseLcov, lineSummary,
  filesFor, prefixFor, checkProvenance, seal, audit, render, gapLocations, ratchetGaps, renderFailure, writeSummary, main,
  identity, digest, readEvidence, parseBranchLcov, validateBranches};
if (require.main === module) process.exitCode = main();
