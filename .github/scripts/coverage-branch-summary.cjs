'use strict';

const fs = require('node:fs');
const file = 'coverage-branch-' + process.env.COVERAGE_TARGET + '.json';
const report = JSON.parse(fs.readFileSync(file, 'utf8'));
if (report.type !== 'llvm.coverage.json.export' || report.data.length !== 1) {
  throw new Error('Unexpected LLVM export schema');
}
const branches = report.data[0].totals.branches;
if (!branches || !Number.isSafeInteger(branches.count) ||
    !Number.isSafeInteger(branches.covered) || branches.count <= 0 ||
    branches.covered < 0 || branches.covered > branches.count) {
  throw new Error('Missing or invalid branch denominator');
}
const percent = (100 * branches.covered / branches.count).toFixed(2);
const lines = [
  '# Diagnostic branch baseline',
  '',
  '- Commit: ' + process.env.GITHUB_SHA,
  '- Target: ' + process.env.COVERAGE_TARGET,
  '- Compiler: nightly-2026-08-14 / LLVM 23.1.0',
  '- Branch outcomes: ' + branches.covered + '/' + branches.count + ' (' + percent + '%)',
  '- Uncovered outcomes: ' + (branches.count - branches.covered),
  '',
  'Informational only. Compiler instrumentation limitations remain applicable.',
  'This report is separate from stable coverage, MC/DC, and Codecov publication.',
  ''
];
fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY, lines.join('\n'));
