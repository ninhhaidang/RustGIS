#!/usr/bin/env bash
# Which packages' tests a CI run needs, as Cargo's package arguments: `packages=--workspace` or
# `packages=-p a -p b …`, on stdout for $GITHUB_OUTPUT (what it chose and why goes to the job
# summary and stderr). Run from a checkout with the base branch's history.
#
# A pull request tests the packages it changes and those that depend on them, as
# `cargo xtask changed --list` finds them (xtask/src/changed.rs). The whole workspace is tested
# for: a push to main; a manual run on every platform; a pull request labelled `ci:macos`,
# `ci:windows` or `ci:full`; a change to Cargo.lock, rust-toolchain*, .github/ or .config/; a file
# outside every package that nothing includes; and anything this can't work out.
#
# From the event: EVENT (github.event_name), BASE_REF (github.base_ref, a pull request's base
# branch), LABELS (the pull request's labels, as JSON) and PLATFORMS (a manual run's choice). A
# manual run on Linux alone (a fork checking a branch) and the merge queue (`merge_group`, whose
# commit is main with the queued pull requests on top) compare with main.
set -euo pipefail

# Chooses the whole workspace, for the reason in $1.
everything() {
  echo "packages=--workspace"
  report "Tests on the whole workspace: $1."
  exit 0
}

# Prints $1 to the log and the job summary.
report() {
  echo "$1" >&2
  [ -z "${GITHUB_STEP_SUMMARY:-}" ] || echo "$1" >> "$GITHUB_STEP_SUMMARY"
}

[ "${EVENT:-}" != push ] || everything "a push to main"
if [ "${EVENT:-}" = workflow_dispatch ] && [ "${PLATFORMS:-all}" != linux ]; then
  everything "a manual run on every platform"
fi
if [ "${EVENT:-}" = pull_request ] \
  && jq -e 'any(.[]; . == "ci:macos" or . == "ci:windows" or . == "ci:full")' <<<"${LABELS:-[]}" >/dev/null; then
  everything "the pull request has a ci: label"
fi

base="origin/${BASE_REF:-main}"
git rev-parse --verify --quiet "$base^{commit}" >/dev/null || everything "there is no $base to compare with"
fork_point="$(git merge-base HEAD "$base")" || everything "HEAD has no history in common with $base"

# Files outside every package that change what a build or test does. `cargo xtask changed` already
# treats any file outside the packages as a reason to test everything; this names the ones that
# must, even if a package ever includes one.
if git diff --name-only "$fork_point" HEAD | grep -qE '^(Cargo\.lock|rust-toolchain|\.github/|\.config/)'; then
  everything "Cargo.lock, rust-toolchain, .github/ or .config/ changed"
fi

selected="$(cargo xtask changed --list "$base")" || everything "cargo xtask changed --list failed"
case "$selected" in
  all) everything "a changed file is outside every package and nothing includes it" ;;
  none)
    echo "packages=none"
    report "No Rust package affected by these changes; skipping Rust tests."
    exit 0
    ;;
  "") everything "no changed file found against $base" ;;
esac
echo "packages=$(sed 's/^/-p /' <<<"$selected" | paste -sd' ' -)"
report "Tests on the packages changed and those that depend on them: $(paste -sd' ' - <<<"$selected")."
