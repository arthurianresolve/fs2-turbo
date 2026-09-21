'use strict';

const fs = require('node:fs');
const audit = require('./coverage-audit.cjs');

function main(root = process.cwd(), env = process.env) {
  const target = env.COVERAGE_TARGET;
  const prefix = audit.prefixFor('branch', target);
  const policy = JSON.parse(audit.readRegular(root, '.github/coverage-branch-policy.json'));
  const json = JSON.parse(audit.readRegular(root, prefix + '.json'));
  const parsed = audit.parseBranchLcov(audit.readRegular(root, prefix + '.lcov').toString('utf8'));
  const branches = audit.validateBranches(json, parsed, target, policy);
  if (!/^[a-f0-9]{40}$/.test(env.GITHUB_SHA || '') || !env.GITHUB_STEP_SUMMARY) {
    throw new Error('Expected CI revision and summary path');
  }
  fs.appendFileSync(env.GITHUB_STEP_SUMMARY, [
    '# Native branch coverage gate', '',
    '- Commit: ' + env.GITHUB_SHA,
    '- Target: ' + target,
    '- Branch outcomes: ' + branches.covered + '/' + branches.count + ' (100%)', '',
    'Complete cross-platform evidence requires the separate collector to pass.',
    'Nightly measured branches are not MC/DC or complete compiler-instantiation coverage.', '',
  ].join('\n'));
}
module.exports = {main};
if (require.main === module) main();
