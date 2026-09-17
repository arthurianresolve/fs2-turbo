'use strict';

const fs = require('node:fs');
const path = require('node:path');
if (process.argv[2] === '--check-equivalence') {
  const source = fs.readFileSync('src/windows/stats/modern.rs', 'utf8');
  const expected = [
    'pub(crate) const fn hresult_from_win32(error: u32) -> windows_sys::core::HRESULT {',
    '    ((error & 0xffff) | 0x8007_0000) as windows_sys::core::HRESULT',
    '}',
  ];
  if (source.split(/\r?\n/).slice(99, 102).join('\n') !== expected.join('\n')) {
    throw new Error('HRESULT mutation equivalence requires review after a source change');
  }
  console.log('Equivalent mutation: disjoint low 16-bit error and 0x80070000 make OR and XOR identical.');
  process.exit(0);
}
const reportPath = path.join(process.env.RUNNER_TEMP, 'mutants.out', 'outcomes.json');
const summaryPath = path.join(process.env.RUNNER_TEMP, 'focused-mutation-summary.json');
const report = JSON.parse(fs.readFileSync(reportPath, 'utf8'));
const counters = ['total_mutants', 'caught', 'missed', 'timeout', 'unviable', 'success'];
for (const name of counters) {
  if (!Number.isSafeInteger(report[name]) || report[name] < 0) {
    throw new Error('Invalid mutation counter: ' + name);
  }
}
const baselines = report.outcomes.filter(outcome => outcome.scenario === 'Baseline');
const summary = {
  sha: process.env.GITHUB_SHA,
  toolchain: process.env.RUSTUP_TOOLCHAIN,
  runner_os: process.env.RUNNER_OS,
  scope: process.env.MUTATION_SCOPE,
  mutation_exclusion: process.env.MUTATION_EXCLUSION ?? null,
  ...Object.fromEntries(counters.map(name => [name, report[name]])),
  baseline_passed: baselines.length > 0 && baselines.every(outcome => outcome.summary === 'Success'),
  completed: typeof report.end_time === 'string',
};
fs.writeFileSync(summaryPath, JSON.stringify(summary, null, 2) + '\n');
const table = [
  '| Mutants | Caught | Missed | Unviable | Timeout |',
  '| ---: | ---: | ---: | ---: | ---: |',
  '| ' + [report.total_mutants, report.caught, report.missed, report.unviable, report.timeout].join(' | ') + ' |',
  '',
  'Unviable mutations did not compile; they are not counted as caught.',
].join('\n');
fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY, table + '\n');
if (!summary.baseline_passed || !summary.completed || report.caught === 0 ||
    report.missed !== 0 || report.timeout !== 0 || report.success !== 0 ||
    report.total_mutants !== report.caught + report.unviable ||
    report.outcomes.length !== report.total_mutants + baselines.length) {
  throw new Error('Focused mutation evidence is incomplete or contains unresolved outcomes');
}
