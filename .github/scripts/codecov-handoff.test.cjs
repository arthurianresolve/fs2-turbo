'use strict';

const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test } = require('node:test');

const workflow = readFileSync(join(__dirname, '..', 'workflows', 'ci.yml'), 'utf8');
const platforms = [
  ['linux', 'x86_64-unknown-linux-gnu'],
  ['windows', 'x86_64-pc-windows-msvc'],
  ['macos', 'aarch64-apple-darwin'],
];

function jobSource(name) {
  const lines = workflow.split(/\r?\n/);
  const start = lines.indexOf(`  ${name}:`);
  assert.notEqual(start, -1, `missing job ${name}`);
  const end = lines.findIndex((line, index) => index > start && /^  [\w-]+:\s*$/.test(line));
  return lines.slice(start, end === -1 ? undefined : end).join('\n');
}

// Deliberately check the workflow's indentation: step labels are not action inputs.
function input(step, name) {
  return step.match(new RegExp(`^ {10}${name}: (.+)$`, 'm'))?.[1].trim();
}

function validateHandoff(job, flagPrefix) {
  const steps = job.split(/(?=^ {6}- )/m).slice(1);
  const downloads = steps.filter(step => /(?:^ {6}- |^ {8})uses: actions\/download-artifact@/m.test(step));
  const tooling = downloads.filter(step => input(step, 'path')?.startsWith('coverage/coverage-tooling-'));
  assert.equal(tooling.length, platforms.length, 'exactly three tooling artifacts are required');
  const audit = steps.findIndex(step => /^ {8}run: node \.github\/scripts\/coverage-audit\.cjs collect tooling coverage$/m.test(step));
  assert.notEqual(audit, -1, 'tooling evidence must be revalidated');

  for (const [os, target] of platforms) {
    const directory = `coverage/coverage-tooling-${target}`;
    const matches = tooling.filter(step => input(step, 'path') === directory);
    assert.equal(matches.length, 1, `${target}: one exact download destination is required`);
    const download = matches[0];
    assert.equal(input(download, 'name'), `coverage-tooling-${target}-attempt-\${{ github.run_attempt }}`, `${target}: artifact name must identify the current attempt`);
    assert.ok(steps.indexOf(download) < audit, `${target}: download must precede audit`);
    const uploads = steps.filter(step => /(?:^ {6}- |^ {8})uses: codecov\/codecov-action@/m.test(step) && input(step, 'flags') === `${flagPrefix}-${os}`);
    assert.equal(uploads.length, 1, `${target}: one scoped Codecov upload is required`);
    const upload = uploads[0];
    assert.equal(input(upload, 'files'), `${directory}/coverage-tooling-${target}.lcov`, `${target}: upload must use the downloaded LCOV`);
    assert.ok(steps.indexOf(upload) > audit, `${target}: audit must precede upload`);
    assert.equal(input(upload, 'disable_search'), 'true', `${target}: implicit report discovery is forbidden`);
    assert.equal(input(upload, 'fail_ci_if_error'), 'true', `${target}: upload errors must fail CI`);
  }
}

for (const [name, prefix] of [['codecov_validation', 'tooling-pilot'], ['codecov', 'tooling']]) {
  const job = jobSource(name);
  test(`${name}: exact-attempt tooling evidence reaches its scoped uploads`, () => {
    validateHandoff(job, prefix);
  });

  for (const [os, target] of platforms) {
    test(`${name}: rejects ${os} display labels used as artifact names`, () => {
      const changed = job.replace(`name: coverage-tooling-${target}-attempt-\${{ github.run_attempt }}`, `name: Download fs2-dev ${os} coverage`);
      assert.notEqual(changed, job, 'regression fixture must change the artifact input');
      assert.throws(() => validateHandoff(changed, prefix), /artifact name must identify the current attempt/);
    });
  }

  test(`${name}: rejects a stale artifact attempt`, () => {
    const changed = job.replace('name: coverage-tooling-x86_64-unknown-linux-gnu-attempt-${{ github.run_attempt }}', 'name: coverage-tooling-x86_64-unknown-linux-gnu-attempt-1');
    assert.notEqual(changed, job);
    assert.throws(() => validateHandoff(changed, prefix), /artifact name must identify the current attempt/);
  });

  test(`${name}: rejects an upload detached from its downloaded report`, () => {
    const changed = job.replace('files: coverage/coverage-tooling-x86_64-unknown-linux-gnu/coverage-tooling-x86_64-unknown-linux-gnu.lcov', 'files: unrelated.lcov');
    assert.notEqual(changed, job);
    assert.throws(() => validateHandoff(changed, prefix), /upload must use the downloaded LCOV/);
  });
}
