'use strict';

const path = require('node:path');
const cp = require('node:child_process');
const audit = require('./coverage-audit.cjs');

function requireValue(value, message) { if (!value) throw new Error(message); }
function key(value) { return JSON.stringify([value.symbol ?? value.name, value.files ?? value.filenames]); }
function normalized(bytes) { return Buffer.from(bytes.toString('utf8').replaceAll('\r\n', '\n')); }
function offset(bytes, line, column) {
  const lines = bytes.toString('utf8').split('\n');
  requireValue(Number.isSafeInteger(line) && line > 0 && line <= lines.length
    && Number.isSafeInteger(column) && column > 0
    && column <= Buffer.byteLength(lines[line - 1]) + 1, 'Source coordinate outside immutable source');
  return Buffer.byteLength(lines.slice(0, line - 1).join('\n')) + (line > 1 ? 1 : 0) + column - 1;
}
function contains(outer, inner) {
  return (inner[0] > outer[0] || inner[0] === outer[0] && inner[1] >= outer[1])
    && (inner[2] < outer[2] || inner[2] === outer[2] && inner[3] <= outer[3]);
}
function definition(entries, index) {
  const spans = new Map();
  for (const entry of entries) {
    const regions = entry.regions.filter(region => region[5] === index && region[7] === 0);
    const containers = regions.filter(region => regions.every(other => contains(region, other)));
    requireValue(containers.length > 0, 'No complete source-definition span');
    for (const region of containers) spans.set(JSON.stringify(region.slice(0, 4)), region.slice(0, 4));
  }
  requireValue(spans.size === 1, 'Ambiguous compiler/source-definition mapping');
  return [...spans.values()][0];
}
function sourceBody(source, range) {
  return source.subarray(offset(source, range[0], range[1]), offset(source, range[2], range[3]));
}
function uniqueBody(source, body) {
  const first = source.indexOf(body);
  return body.length > 0 && first >= 0 && source.indexOf(body, first + 1) < 0;
}
function propose(oldReport, newReport, readSource) {
  for (const report of [oldReport, newReport]) requireValue(report.type === 'llvm.coverage.json.export'
    && report.data?.length === 1, 'Invalid LLVM export envelope');
  requireValue(oldReport.version === newReport.version, 'Exporter/schema changes require manual review');
  const oldData = oldReport.data[0], newData = newReport.data[0];
  for (const metric of audit.METRICS) audit.ratchet(newData.totals?.[metric], oldData.totals?.[metric], metric);
  const oldGaps = audit.gapLocations(oldData), newGaps = audit.gapLocations(newData);
  const oldGroups = new Map(oldGaps.map(group => [key(group), group]));
  const index = data => {
    const groups = new Map();
    for (const fn of data.functions) {
      const name = key(fn);
      if (!groups.has(name)) groups.set(name, []);
      groups.get(name).push(fn);
    }
    return groups;
  };
  const oldDefinitions = index(oldData), newDefinitions = index(newData), proofs = [], mapped = [];
  for (const current of newGaps) {
    const name = key(current), previous = oldGroups.get(name);
    requireValue(previous, 'New or changed symbol/file identity requires manual review');
    try { audit.ratchetGaps([current], [previous], 'relocation'); mapped.push(previous); continue; }
    catch { /* Only a proved relocation can reconcile a changed coordinate. */ }
    const byFile = new Map();
    for (let fileId = 0; fileId < previous.files.length; fileId++) {
      const file = previous.files[fileId];
      const oldRegions = previous.regions.filter(region => region[4] === fileId);
      const newRegions = current.regions.filter(region => region[4] === fileId);
      const unchanged = {...previous, regions: oldRegions, missed_entries: 0};
      try {
        audit.ratchetGaps([{...current, regions: newRegions, missed_entries: 0}], [unchanged], 'relocation');
        continue;
      } catch { /* This file needs an exact unchanged named-function proof. */ }
      const safe = audit.sourcePath(file);
      requireValue(safe.startsWith('src/') && safe.endsWith('.rs'), 'External/compiler paths cannot be relocated');
      const before = definition(oldDefinitions.get(name), fileId);
      const after = definition(newDefinitions.get(name), fileId);
      const shift = after[0] - before[0];
      requireValue(before[1] === after[1] && before[3] === after[3]
        && after[2] - before[2] === shift, 'Only whole-line definition relocation is supported');
      const oldSource = normalized(readSource('old', safe)), newSource = normalized(readSource('new', safe));
      const oldBody = sourceBody(oldSource, before), newBody = sourceBody(newSource, after);
      requireValue(oldBody.equals(newBody) && /\bfn\s+[A-Za-z_][A-Za-z0-9_]*/.test(oldBody.toString('utf8')),
        'Changed or unnamed function body requires manual review');
      requireValue(uniqueBody(oldSource, oldBody) && uniqueBody(newSource, newBody), 'Ambiguous repeated function body');
      requireValue(oldRegions.every(region => contains(before, region)), 'Gap lies outside the unchanged definition');
      byFile.set(fileId, shift);
      proofs.push({symbol: previous.symbol, file: safe, old_span: before, new_span: after,
        line_delta: shift, unchanged_body_sha256: audit.digest(oldBody),
        old_source_sha256: audit.digest(oldSource), new_source_sha256: audit.digest(newSource)});
    }
    mapped.push({...previous, regions: previous.regions.map(region => {
      const shift = byFile.get(region[4]) || 0;
      return [region[0] + shift, region[1], region[2] + shift, region[3], region[4]];
    })});
  }
  audit.ratchetGaps(newGaps, mapped, 'reviewed relocation');
  return {status: 'proposal-only', requires_review: true, proofs, proposed_reviewed_gaps: newGaps};
}
function main(args = process.argv.slice(2), root = process.cwd()) {
  try {
    const [target, candidateSha, baselineJson, directory] = args;
    requireValue(args.length === 4 && audit.TARGETS.primary.includes(target) && /^[a-f0-9]{40}$/.test(candidateSha || ''),
      'Usage: coverage-relocate.cjs TARGET CANDIDATE_SHA BASELINE_JSON CANDIDATE_ARTIFACT_DIRECTORY');
    const policyBytes = audit.readRegular(root, '.github/coverage-policy.json');
    const policy = JSON.parse(policyBytes), baseline = policy.targets?.[target];
    requireValue(/^[a-f0-9]{40}$/.test(policy.baseline_sha || '') && baseline?.gap_provenance,
      'Missing reviewed baseline identity');
    const baselinePath = path.resolve(root, baselineJson);
    const oldBytes = audit.readRegular(path.dirname(baselinePath), path.basename(baselinePath));
    requireValue(audit.digest(oldBytes) === baseline.gap_provenance.json_sha256, 'Input is not the reviewed baseline JSON');
    const artifacts = path.resolve(root, directory), prefix = audit.prefixFor('primary', target);
    const receipt = JSON.parse(audit.readRegular(path.join(artifacts, prefix), prefix + '-receipt.json'));
    const git = (...values) => cp.execFileSync('git', values, {cwd: root, maxBuffer: 64 * 1024 * 1024});
    const tree = git('rev-parse', candidateSha + '^{tree}').toString('utf8').trim();
    requireValue(/^[1-9]\d*$/.test(receipt.run) && /^[1-9]\d*$/.test(receipt.attempt), 'Invalid candidate run identity');
    const expected = {sha: candidateSha, tree, run: receipt.run, attempt: receipt.attempt};
    const verified = audit.readEvidence(artifacts, 'primary', target, expected, policy);
    const newBytes = Buffer.from(verified.evidence.get(prefix + '.json'));
    const oldReport = JSON.parse(oldBytes), newReport = JSON.parse(newBytes);
    requireValue(oldReport.version === baseline.gap_provenance.export_version
      && baseline.gap_provenance.toolchain === policy.toolchain
      && baseline.gap_provenance.llvm_cov === policy.llvm_cov, 'Reviewed compiler/export identity mismatch');
    const manifest = new Map();
    for (const line of verified.evidence.get(prefix + '-source.sha256').trimEnd().split(/\r?\n/)) {
      const match = /^([a-f0-9]{64}) [ *](.+)$/.exec(line);
      requireValue(match, 'Invalid candidate source manifest');
      const file = audit.sourcePath(match[2]);
      requireValue(!manifest.has(file), 'Duplicate candidate source manifest entry');
      manifest.set(file, match[1]);
    }
    const readSource = (side, file) => {
      const bytes = git('show', (side === 'old' ? policy.baseline_sha : candidateSha) + ':' + file);
      if (side === 'new') {
        const lf = normalized(bytes);
        requireValue([audit.digest(bytes), audit.digest(lf),
          audit.digest(lf.toString('utf8').replaceAll('\n', '\r\n'))].includes(manifest.get(file)),
        'Candidate source digest differs from the immutable commit');
      }
      return bytes;
    };
    const result = propose(oldReport, newReport, readSource);
    console.log(JSON.stringify({schema: 1, ...result, target, baseline_sha: policy.baseline_sha,
      candidate_sha: candidateSha, candidate_run: receipt.run, candidate_attempt: receipt.attempt,
      policy_sha256: audit.digest(policyBytes), baseline_json_sha256: audit.digest(oldBytes),
      candidate_json_sha256: audit.digest(newBytes), candidate_receipt_sha256: verified.receipt,
      note: 'Review-only proposal. No policy, source, report, or CI verdict was changed. Review all native evidence before adoption.'}, null, 2));
    return 0;
  } catch (error) { console.error(error.message); return 1; }
}
module.exports = {propose, main};
if (require.main === module) process.exitCode = main();
