'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const audit = require('./coverage-audit.cjs');

const lcov = (hits = 1, found = 1) => 'SF:src/lib.rs\nDA:1,' + hits + '\nLF:' + found + '\nLH:' + (hits ? found : 0) + '\nend_of_record\n';
test('uses physical DA coordinates and preserves aggregate-summary differences', () => {
  const result = audit.parseLcov(lcov(1, 2));
  assert.deepEqual(result.totals, {count: 1, covered: 1});
  assert.equal(result.summaries.length, 1);
});
test('normalizes Windows source coordinates', () => {
  assert.ok(audit.parseLcov(lcov().replace('src/lib.rs', 'src\\lib.rs')).lines.has('src/lib.rs'));
});
for (const [label, input] of [
  ['duplicate coordinate', lcov().replace('LF:', 'DA:1,1\nLF:')],
  ['duplicate source', lcov() + lcov()],
  ['missing end', lcov().replace('end_of_record', '')],
  ['missing summary', lcov().replace('LF:1\n', '')],
  ['negative count', lcov().replace('DA:1,1', 'DA:1,-1')],
  ['traversal', lcov().replace('src/lib.rs', '../outside.rs')],
  ['absolute path', lcov().replace('src/lib.rs', '/src/lib.rs')],
  ['branch input', lcov().replace('LF:', 'BRDA:1,0,0,1\nLF:')],
  ['empty report', ''],
]) test('rejects ' + label, () => assert.throws(() => audit.parseLcov(input)));
test('ratchet uses exact counts rather than rounded percentages', () => {
  assert.throws(() => audit.ratchet({covered: 999999, count: 1000000}, {covered: 1, count: 1}, 'lines'));
  audit.ratchet({covered: 10, count: 10}, {covered: 9, count: 10}, 'lines');
  assert.throws(() => audit.ratchet({covered: 18, count: 20}, {covered: 9, count: 10}, 'missed count'));
});
function fixture(t, profile = 'primary') {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'fs2-coverage-fixture-'));
  t.after(() => {
    assert.equal(path.dirname(path.resolve(root)), path.resolve(os.tmpdir()));
    assert.ok(path.basename(root).startsWith('fs2-coverage-fixture-'));
    fs.rmSync(root, {recursive: true, force: true});
  });
  fs.mkdirSync(path.join(root, 'src'));
  const source = 'pub fn covered() {}\n';
  fs.writeFileSync(path.join(root, 'src/lib.rs'), source);
  const sourceFile = profile === 'tooling' ? 'tools/fs2-dev/src/main.rs' : 'src/lib.rs';
  if (profile === 'tooling') {
    fs.mkdirSync(path.join(root, 'tools/fs2-dev/src'), {recursive: true});
    fs.writeFileSync(path.join(root, sourceFile), source);
  }
  const expected = {sha: 'a'.repeat(40), tree: 'b'.repeat(40), run: '123', attempt: '1', sources: ['src/lib.rs', ...(profile === 'tooling' ? [sourceFile] : [])]};
  const counts = {count: 1, covered: 1};
  const branch = profile === 'branch';
  const toolchain = profile === 'msrv' ? '1.88.0' : branch ? '1.99.0-nightly' : '1.98.1';
  const totals = Object.fromEntries(audit.METRICS.map(metric => [metric, counts]));
  if (branch) totals.branches = {count: 2, covered: 2, notcovered: 0};
  const policy = {schema: 1, toolchain: branch ? toolchain : '1.98.1',
    compiler_commit: 'd'.repeat(40), llvm_version: '23.1.0', export_version: '3.1.0',
    llvm_cov: '0.8.7', baseline_sha: 'c'.repeat(40),
    source_inventory: ['src/lib.rs'], tooling_inventory: profile === 'tooling' ? [sourceFile] : [], targets: {}};
  const data = {type: 'llvm.coverage.json.export', version: '3.1.0', data: [{
    totals, files: [{filename: sourceFile, ...(branch ? {
      branches: [[1, 4, 1, 8, 1, 1, 0, 0, 4]], summary: {branches: totals.branches},
    } : {})}], functions: [{name: 'covered', filenames: [sourceFile], count: 1, regions: [[1, 1, 1, 20, 1, 0, 0, 0]]}],
  }]};
  const diagnostics = target => ({schema_version: 4, target,
    intended_integration_definitions: counts, profiles: [{
      profile: 'combined', json_entries: counts, source_definitions: counts,
      llvm_instantiations: counts, definitions: [{covered_entries: 1}],
    }]});
  const directory = path.join(root, 'inputs');
  fs.mkdirSync(directory);
  for (const target of audit.TARGETS[profile]) {
    const prefix = audit.prefixFor(profile, target), folder = path.join(directory, prefix);
    fs.mkdirSync(folder);
    policy.targets[target] = {reported_files: ['src/lib.rs'], llvm_totals: totals,
      json_entries: counts, source_definitions: counts, intended_integration_definitions: counts, reviewed_gaps: [],
      gap_provenance: {toolchain: '1.98.1', llvm_cov: '0.8.7', export_version: '3.1.0'}};
    if (branch) Object.assign(policy.targets[target], {
      outcomes: 2, locations: [[sourceFile, 1, 4, 1, 8]], runner_label: 'fixture',
    });
    const provenance = [
      'requested_sha=' + expected.sha, 'checked_out_sha=' + expected.sha, 'tree=' + expected.tree,
      'run_id=123', 'run_attempt=1', 'target=' + target, 'toolchain=' + toolchain, 'CARGO_INCREMENTAL=0',
      'host: ' + target, 'release: ' + toolchain, 'cargo-llvm-cov 0.8.7',
      ...(branch ? ['commit-hash: ' + policy.compiler_commit, 'LLVM version: ' + policy.llvm_version,
        'runner_label=fixture', 'runner_image_os=fixture', 'runner_image_version=1.0', 'node_version=v26.5.0'] : []),
    ].join('\n');
    for (const name of audit.filesFor(profile, target)) {
      let content = 'diagnostic\n';
      if (name.endsWith('.lcov')) content = lcov().replace('src/lib.rs', sourceFile);
      else if (name.endsWith('-provenance.txt')) content = provenance;
      else if (name.endsWith('-source.sha256')) content = crypto.createHash('sha256').update(source).digest('hex') + '  ' + sourceFile + '\n';
      else if (name.endsWith('-codecov.json')) content = JSON.stringify({coverage: {[sourceFile]: {'1': 1}}});
      else if (name.includes('diagnostics-')) content = JSON.stringify(diagnostics(target));
      else if (name.endsWith('.json')) content = JSON.stringify(data);
      if (branch && name.endsWith('.lcov')) {
        content = content.replace('LF:', 'BRDA:1,0,0,1\nBRDA:1,0,1,1\nBRF:2\nBRH:2\nLF:');
      }
      fs.writeFileSync(path.join(folder, name), content);
    }
    audit.seal(folder, profile, target, expected);
  }
  const target = audit.TARGETS[profile][0], prefix = audit.prefixFor(profile, target);
  const folder = path.join(directory, prefix);
  const run = () => audit.audit(root, directory, profile, expected, policy);
  const reseal = () => {
    fs.unlinkSync(path.join(folder, prefix + '-receipt.json'));
    audit.seal(folder, profile, target, expected);
  };
  return {root, directory, expected, policy, folder, prefix, run, reseal, profile, data};
}
test('three-platform merge counts shared source only once', t => {
  const f = fixture(t), result = f.run();
  assert.deepEqual(result.merged_unique_lines, {count: 1, covered: 1});
  assert.equal(result.platforms.length, 3);
  assert.equal(result.sources_without_line_records.length, 0);
  assert.match(audit.render(result), /Instantiations/);
});
test('missing platform cannot produce a complete green result', t => {
  const f = fixture(t);
  fs.renameSync(f.folder, f.folder + '-missing');
  assert.throws(f.run);
});
test('modified report cannot bypass its digest receipt', t => {
  const f = fixture(t);
  fs.appendFileSync(path.join(f.folder, f.prefix + '.lcov'), '\n');
  assert.throws(f.run, /digest mismatch/);
});
test('stale revision and attempts are rejected', t => {
  const f = fixture(t);
  f.expected.attempt = '2';
  assert.throws(f.run, /Stale\/mixed/);
});
test('new source files require inventory review', t => {
  const f = fixture(t);
  f.expected.sources.push('src/new.rs');
  assert.throws(f.run, /inventory requires review/);
});
test('line misses cannot be masked by another platform', t => {
  const f = fixture(t);
  fs.writeFileSync(path.join(f.folder, f.prefix + '.lcov'), lcov(0));
  f.reseal();
  assert.throws(f.run, /below 100%/);
});
test('source digest binds a report to its actual checkout', t => {
  const f = fixture(t);
  fs.writeFileSync(path.join(f.root, 'src/lib.rs'), 'pub fn changed() {}\n');
  assert.throws(f.run, /Source digest mismatch/);
});
test('legitimate CRLF checkout differences are accepted', t => {
  const f = fixture(t);
  fs.writeFileSync(path.join(f.root, 'src/lib.rs'), 'pub fn covered() {}\r\n');
  assert.equal(f.run().platforms.length, 3);
});
test('compiler identity cannot differ despite valid report digests', t => {
  const f = fixture(t);
  const file = path.join(f.folder, f.prefix + '-provenance.txt');
  fs.writeFileSync(file, fs.readFileSync(file, 'utf8').replace('release: 1.98.1', 'release: 1.88.0'));
  f.reseal();
  assert.throws(f.run, /Compiler or exporter/);
});
test('missing pilot output is not a successful pilot', t => {
  const f = fixture(t);
  fs.unlinkSync(path.join(f.folder, f.prefix + '-codecov.json'));
  assert.throws(f.run);
});
test('a receipt is never silently overwritten', t => {
  const f = fixture(t);
  assert.throws(() => audit.seal(f.folder, 'primary', audit.TARGETS.primary[0], f.expected));
});
for (const [label, type] of [
  ['directory symlink', 'dir'],
  ...(process.platform === 'win32' ? [['Windows junction', 'junction']] : []),
]) {
  test(label + ' artifact roots are rejected', t => {
    const f = fixture(t);
    fs.renameSync(f.folder, f.folder + '-real');
    fs.symlinkSync(f.folder + '-real', f.folder, type);
    assert.ok(fs.lstatSync(f.folder).isSymbolicLink());
    assert.throws(f.run, /Linked artifact/);
  });
}

for (const profile of Object.keys(audit.TARGETS)) {
  test(profile + ': complete fixture collection preserves its own profile', t => {
    const f = fixture(t, profile), report = f.run();
    assert.equal(report.profile, profile);
    assert.equal(report.status, 'complete');
    assert.equal(report.toolchain, profile === 'msrv' ? '1.88.0' : profile === 'branch' ? '1.99.0-nightly' : '1.98.1');
    assert.equal(report.platforms.length, audit.TARGETS[profile].length);
    assert.equal(report.platforms.every(p => p.missed_locations.length === 0), true);
  });
  for (const fault of ['empty', 'malformed', 'missing target', 'earlier attempt', 'wrong exporter', 'wrong compiler']) {
    test(profile + ': rejects ' + fault + ' evidence', t => {
      const f = fixture(t, profile);
      if (fault === 'empty' || fault === 'malformed') {
        fs.writeFileSync(path.join(f.folder, f.prefix + '.json'), fault === 'empty' ? '' : '{');
        if (fault === 'malformed') f.reseal();
      } else if (fault === 'missing target') {
        fs.renameSync(f.folder, f.folder + '-absent');
      } else if (fault === 'earlier attempt') {
        f.expected.attempt = '2';
      } else {
        const file = path.join(f.folder, f.prefix + '-provenance.txt');
        const content = fs.readFileSync(file, 'utf8');
        fs.writeFileSync(file, fault === 'wrong exporter'
          ? content.replace('cargo-llvm-cov 0.8.7', 'cargo-llvm-cov 0.8.6')
          : content.replace('release: ', 'wrong-release: '));
        f.reseal();
      }
      assert.throws(f.run);
    });
  }
}
test('gap identity detects a swapped miss despite equal numeric counters', () => {
  const fn = (name, count) => ({name, filenames: ['src/lib.rs'], count,
    regions: [[1, 1, 1, 20, count, 0, 0, 0]]});
  const before = audit.gapLocations({functions: [fn('first', 0), fn('second', 1)]});
  const after = audit.gapLocations({functions: [fn('first', 1), fn('second', 0)]});
  audit.ratchet({count: 2, covered: 1}, {count: 2, covered: 1}, 'unchanged totals');
  assert.throws(() => audit.ratchetGaps(after, before, 'native'), /missed location/);
});
test('region moves and extra duplicate gaps require review; closing gaps is allowed', () => {
  const fn = {name: 'generic', filenames: ['src/lib.rs'], count: 1,
    regions: [[1, 1, 1, 20, 0, 0, 0, 0]]};
  const old = audit.gapLocations({functions: [fn]});
  audit.ratchetGaps([], old, 'native');
  assert.throws(() => audit.ratchetGaps(audit.gapLocations({functions: [fn, fn]}), old, 'native'));
  assert.throws(() => audit.ratchetGaps(audit.gapLocations({functions: [{
    ...fn, regions: [[2, 1, 2, 20, 0, 0, 0, 0]],
  }]}), old, 'native'));
});
test('external compiler entries remain visible without opening their paths', () => {
  const gaps = audit.gapLocations({functions: [{
    name: 'compiler_entry', filenames: ['/rustc/compiler/library/file.rs'], count: 0,
    regions: [[10, 1, 10, 3, 0, 0, 0, 0]],
  }]});
  assert.equal(gaps[0].missed_entries, 1);
  assert.equal(gaps[0].files[0], '/rustc/compiler/library/file.rs');
});
test('invalid LLVM region/file identity fails closed', () => {
  assert.throws(() => audit.gapLocations({functions: [{
    name: 'broken', filenames: ['src/lib.rs'], count: 0, regions: [[1, 1, 1, 20, 0, 5, 0, 0]],
  }]}));
});
test('publisher input ignores unrelated supplemental artifacts', t => {
  const f = fixture(t);
  fs.mkdirSync(path.join(f.directory, 'coverage-tooling-unrelated'));
  assert.equal(f.run().platforms.length, 3);
});
test('path replacement during reading is rejected', t => {
  const f = fixture(t), file = path.join(f.folder, f.prefix + '.txt');
  const read = fs.readFileSync;
  let replaced = false;
  t.mock.method(fs, 'readFileSync', function (value, ...args) {
    const bytes = read.call(fs, value, ...args);
    if (typeof value === 'number' && !replaced) {
      replaced = true;
      fs.renameSync(file, file + '.original');
      fs.writeFileSync(file, 'replacement\n');
    }
    return bytes;
  });
  assert.throws(() => audit.readRegular(f.folder, f.prefix + '.txt'), /replaced while reading/);
});
function outputs(f) {
  fs.mkdirSync(path.join(f.root, '.github'));
  fs.writeFileSync(path.join(f.root, f.profile === 'branch'
    ? '.github/coverage-branch-policy.json' : '.github/coverage-policy.json'), JSON.stringify(f.policy));
  return {RUNNER_TEMP: f.root, GITHUB_STEP_SUMMARY: path.join(f.root, 'step-summary.md'),
    GITHUB_OUTPUT: path.join(f.root, 'step-output.txt'), GITHUB_SHA: f.expected.sha,
    GITHUB_RUN_ID: f.expected.run, GITHUB_RUN_ATTEMPT: f.expected.attempt, COVERAGE_JOB_RESULT: 'success'};
}
for (const profile of Object.keys(audit.TARGETS)) {
  test(profile + ': collection entrypoint emits a complete summary', t => {
    const f = fixture(t, profile), env = outputs(f);
    assert.equal(audit.main(['collect', profile, f.directory], f.root, env, () => f.expected), 0);
    assert.match(fs.readFileSync(env.GITHUB_STEP_SUMMARY, 'utf8'), /Coverage evidence:/);
    assert.match(fs.readFileSync(env.GITHUB_OUTPUT, 'utf8'), /report_directory=/);
  });
}
test('failed collection retains all missing-target diagnostics and exits nonzero', t => {
  const f = fixture(t), env = outputs(f);
  for (const target of audit.TARGETS.primary) {
    const folder = path.join(f.directory, audit.prefixFor('primary', target));
    fs.renameSync(folder, folder + '-missing');
  }
  assert.equal(audit.main(['collect', 'primary', f.directory], f.root, env, () => f.expected), 1);
  const output = fs.readFileSync(env.GITHUB_OUTPUT, 'utf8').trim().slice('report_directory='.length);
  const summary = JSON.parse(fs.readFileSync(path.join(output, 'summary.json')));
  assert.equal(summary.status, 'rejected');
  assert.equal(summary.failures.length, 3);
  assert.equal(summary.merged_unique_lines, undefined);
  assert.match(fs.readFileSync(env.GITHUB_STEP_SUMMARY, 'utf8'), /Re-run all jobs/);
});
test('failed producer jobs cannot publish otherwise complete artifacts', t => {
  const f = fixture(t), env = outputs(f);
  env.COVERAGE_JOB_RESULT = 'failure';
  assert.equal(audit.main(['collect', 'primary', f.directory], f.root, env, () => f.expected), 1);
  assert.match(fs.readFileSync(env.GITHUB_STEP_SUMMARY, 'utf8'), /did not all succeed/);
});
test('failure summaries escape artifact-controlled markup', () => {
  const text = audit.renderFailure({failures: [{message: '<script>bad</script>'}]});
  assert.ok(!text.includes('<script>'));
  assert.match(text, /&lt;script&gt;/);
});

test('branch gate retains its independent denominator and never exports a Codecov pilot', t => {
  const f = fixture(t, 'branch'), report = f.run();
  assert.ok(report.platforms.every(platform => platform.branches.count === 2 && platform.branches.covered === 2));
  assert.ok(!audit.filesFor('branch', audit.TARGETS.branch[0]).some(file => file.includes('codecov')));
  assert.match(audit.render(report), /not measure every Rust construct/);
});
for (const fault of ['miss', 'forged full total', 'zero denominator', 'coordinate drift', 'denominator drift',
  'duplicate source', 'malformed region', 'lcov mismatch', 'wrong nightly', 'missing runner', 'wrong runner']) {
  test('branch gate rejects ' + fault, t => {
    const f = fixture(t, 'branch'), data = structuredClone(f.data), target = audit.TARGETS.branch[0];
    if (fault === 'miss') { data.data[0].totals.branches.covered = 1; data.data[0].totals.branches.notcovered = 1; }
    if (fault === 'forged full total') data.data[0].files[0].branches[0][4] = 0;
    if (fault === 'zero denominator') data.data[0].totals.branches = {count: 0, covered: 0, notcovered: 0};
    if (fault === 'coordinate drift') data.data[0].files[0].branches[0][1]++;
    if (fault === 'denominator drift') f.policy.targets[target].outcomes = 4;
    if (fault === 'duplicate source') data.data[0].files.push(data.data[0].files[0]);
    if (fault === 'malformed region') data.data[0].files[0].branches[0][8] = 5;
    fs.writeFileSync(path.join(f.folder, f.prefix + '.json'), JSON.stringify(data));
    if (fault === 'lcov mismatch') {
      const file = path.join(f.folder, f.prefix + '.lcov');
      fs.writeFileSync(file, fs.readFileSync(file, 'utf8').replace('BRH:2', 'BRH:1'));
    }
    if (fault === 'wrong nightly' || fault === 'missing runner' || fault === 'wrong runner') {
      const file = path.join(f.folder, f.prefix + '-provenance.txt');
      let text = fs.readFileSync(file, 'utf8');
      if (fault === 'wrong nightly') text = text.replace('commit-hash: ' + f.policy.compiler_commit, 'commit-hash: ' + 'e'.repeat(40));
      if (fault === 'missing runner') text = text.replace('node_version=v26.5.0', 'node_version=unset');
      if (fault === 'wrong runner') text = text.replace('runner_label=fixture', 'runner_label=different');
      fs.writeFileSync(file, text);
    }
    f.reseal();
    assert.throws(f.run);
  });
}
test('branch rows from generic instances are unioned without hiding a missing physical outcome', t => {
  const f = fixture(t, 'branch'), data = structuredClone(f.data);
  data.data[0].files[0].branches = [[1, 4, 1, 8, 1, 0, 0, 0, 4], [1, 4, 1, 8, 0, 1, 0, 0, 4]];
  fs.writeFileSync(path.join(f.folder, f.prefix + '.json'), JSON.stringify(data));
  f.reseal();
  assert.deepEqual(f.run().platforms[0].branches, {count: 2, covered: 2});
});
test('branch LCOV rejects duplicate records and missing outcome summaries', () => {
  const text = lcov().replace('LF:', 'BRDA:1,0,0,1\nBRDA:1,0,1,1\nBRF:2\nBRH:2\nLF:');
  assert.throws(() => audit.parseBranchLcov(text.replace('BRF:', 'BRDA:1,0,0,1\nBRF:')));
  assert.throws(() => audit.parseBranchLcov(text.replace('BRH:2\n', '')));
  assert.throws(() => audit.parseBranchLcov(text.replace('BRDA:1,0,0,1', 'BRDA:1,0,0,-')));
});
test('standalone branch summary applies the same strict gate', t => {
  const f = fixture(t, 'branch');
  fs.mkdirSync(path.join(f.folder, '.github'));
  fs.writeFileSync(path.join(f.folder, '.github/coverage-branch-policy.json'), JSON.stringify(f.policy));
  const env = {COVERAGE_TARGET: audit.TARGETS.branch[0], GITHUB_SHA: f.expected.sha,
    GITHUB_STEP_SUMMARY: path.join(f.root, 'branch-summary.md')};
  require('./coverage-branch-summary.cjs').main(f.folder, env);
  assert.match(fs.readFileSync(env.GITHUB_STEP_SUMMARY, 'utf8'), /2\/2 \(100%\)/);
  assert.throws(() => require('./coverage-branch-summary.cjs').main(f.folder, {...env, COVERAGE_TARGET: '../outside'}));
});
