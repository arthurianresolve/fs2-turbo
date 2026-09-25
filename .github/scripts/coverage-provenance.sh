#!/usr/bin/env bash
set -euo pipefail
umask 077

if [[ $# -ne 2 ]]; then
  echo 'usage: coverage-provenance.sh record|verify coverage-prefix' >&2
  exit 2
fi
mode=$1
prefix=$2
if [[ ! "$prefix" =~ ^coverage-[[:alnum:]_.-]+$ ]]; then
  echo 'coverage evidence prefix must be a local coverage-* basename' >&2
  exit 2
fi

actual_sha=$(git rev-parse HEAD)
if [[ "$actual_sha" != "${GITHUB_SHA:?}" ]]; then
  echo 'checked-out revision differs from the requested coverage revision' >&2
  exit 1
fi
git diff --exit-code HEAD

if command -v sha256sum >/dev/null 2>&1; then
  digest=(sha256sum)
else
  digest=(shasum -a 256)
fi

case "$mode" in
  record)
    set -o noclobber
    compiler=$(rustc -vV)
    host=$(printf '%s\n' "$compiler" | sed -n 's/^host: //p')
    release=$(printf '%s\n' "$compiler" | sed -n 's/^release: //p')
    if [[ "$host" != "${COVERAGE_TARGET:?}" || "$release" != "${COVERAGE_TOOLCHAIN:?}" ]]; then
      echo 'coverage requires the requested native host and exact compiler' >&2
      exit 1
    fi
    git ls-files -z | xargs -0 "${digest[@]}" -- > "$prefix-source.sha256"
    {
      printf 'requested_sha=%s\nchecked_out_sha=%s\n' "$GITHUB_SHA" "$actual_sha"
      printf 'tree=%s\n' "$(git rev-parse 'HEAD^{tree}')"
      printf 'target=%s\ntoolchain=%s\n' "$COVERAGE_TARGET" "$COVERAGE_TOOLCHAIN"
      printf 'run_id=%s\nrun_attempt=%s\n' "${GITHUB_RUN_ID:?}" "${GITHUB_RUN_ATTEMPT:?}"
      printf 'CARGO_INCREMENTAL=%s\n' "${CARGO_INCREMENTAL:-unset}"
      printf 'CARGO_LLVM_COV_TARGET_DIR=%s\n' "${CARGO_LLVM_COV_TARGET_DIR:-unset}"
      printf 'CARGO_PROFILE_DEV_DEBUG=%s\n' "${CARGO_PROFILE_DEV_DEBUG:-default}"
      printf 'CARGO_PROFILE_TEST_DEBUG=%s\n' "${CARGO_PROFILE_TEST_DEBUG:-default}"
      printf 'runner_label=%s\n' "${COVERAGE_RUNNER_LABEL:-unset}"
      printf 'runner_image_os=%s\nrunner_image_version=%s\n' "${ImageOS:-unset}" "${ImageVersion:-unset}"
      printf 'node_version=%s\n' "$(node --version)"
      printf '%s\n' "$compiler"
      cargo --version --locked
      cargo llvm-cov --version --locked
      uname -a
      df -Pk .
    } > "$prefix-provenance.txt"
    ;;
  verify)
    "${digest[@]}" --check -- "$prefix-source.sha256"
    ;;
  *)
    echo 'expected record or verify' >&2
    exit 2
    ;;
esac
