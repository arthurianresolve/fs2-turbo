'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const relocate = require('./coverage-relocate.cjs');

function fixture() {
  const text = 'fn guarded(value: bool) {\n    if value { return; }\n}\n';
  const totals = Object.fromEntries(['lines', 'regions', 'functions', 'instantiations']
    .map(metric => [metric, {count: 10, covered: 9}]));
  const fn = {name: 'guarded', filenames: ['src/test.rs'], count: 1,
    regions: [[1, 1, 3, 2, 1, 0, 0, 0], [2, 16, 2, 23, 0, 0, 0, 0]]};
  const oldReport = {type: 'llvm.coverage.json.export', version: '3.1.0', data: [{totals, functions: [fn]}]};
  const newReport = structuredClone(oldReport);
  for (const region of newReport.data[0].functions[0].regions) { region[0] += 2; region[2] += 2; }
  const sources = {old: Buffer.from(text), new: Buffer.from('// inserted\n\n' + text)};
  const read = (side, file) => { assert.equal(file, 'src/test.rs'); return sources[side]; };
  return {oldReport, newReport, sources, read, run: () => relocate.propose(oldReport, newReport, read)};
}
test('proposes an exact line relocation without accepting or modifying a baseline', () => {
  const f = fixture(), before = JSON.stringify([f.oldReport, f.newReport]);
  const result = f.run();
  assert.equal(result.status, 'proposal-only');
  assert.equal(result.requires_review, true);
  assert.equal(result.proofs.length, 1);
  assert.equal(result.proofs[0].line_delta, 2);
  assert.equal(result.proofs[0].unchanged_body_sha256.length, 64);
  assert.equal(JSON.stringify([f.oldReport, f.newReport]), before);
});
test('normalizes only checkout CRLF for an otherwise identical function', () => {
  const f = fixture();
  f.sources.new = Buffer.from(f.sources.new.toString().replaceAll('\n', '\r\n'));
  assert.equal(f.run().proofs.length, 1);
});
test('unchanged external compiler identities do not open external paths', () => {
  const f = fixture();
  f.oldReport.data[0].functions[0].filenames = ['/rustc/library/core.rs'];
  f.newReport = structuredClone(f.oldReport);
  assert.equal(relocate.propose(f.oldReport, f.newReport, () => assert.fail('external path opened')).proofs.length, 0);
});
test('closing gaps needs no relocation waiver', () => {
  const f = fixture();
  f.newReport.data[0].functions[0].regions[1][4] = 1;
  assert.deepEqual(f.run().proposed_reviewed_gaps, []);
});
for (const fault of ['changed body', 'repeated body', 'symbol', 'filename', 'new miss', 'duplicate miss',
  'missed entry', 'exporter', 'metric regression', 'column movement', 'out of bounds', 'external relocation']) {
  test('rejects ' + fault, () => {
    const f = fixture(), fn = f.newReport.data[0].functions[0];
    if (fault === 'changed body') f.sources.new = Buffer.from(f.sources.new.toString().replace('if value', 'if false'));
    if (fault === 'repeated body') f.sources.new = Buffer.concat([f.sources.new, f.sources.old]);
    if (fault === 'symbol') fn.name = 'different';
    if (fault === 'filename') fn.filenames = ['src/other.rs'];
    if (fault === 'new miss') fn.regions[1][1]++;
    if (fault === 'duplicate miss') fn.regions.push([...fn.regions[1]]);
    if (fault === 'missed entry') fn.count = 0;
    if (fault === 'exporter') f.newReport.version = '4.0.0';
    if (fault === 'metric regression') f.newReport.data[0].totals.regions.covered--;
    if (fault === 'column movement') fn.regions[0][1]++;
    if (fault === 'out of bounds') { fn.regions[0][2] += 100; fn.regions[1][2] += 100; }
    if (fault === 'external relocation') {
      f.oldReport.data[0].functions[0].filenames = ['/rustc/library/core.rs'];
      fn.filenames = ['/rustc/library/core.rs'];
    }
    assert.throws(f.run);
  });
}
test('ambiguous compiler definition instances are rejected', () => {
  const f = fixture(), extra = structuredClone(f.oldReport.data[0].functions[0]);
  extra.regions[0][1] = 2;
  f.oldReport.data[0].functions.push(extra);
  assert.throws(f.run, /Ambiguous/);
});
