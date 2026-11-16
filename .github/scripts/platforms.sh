#!/usr/bin/env bash
# The platforms to test on, as a JSON list of runners. Pushes to main and releases test on every
# platform, and so does a manual run unless it asks for `linux` (PLATFORMS), which tests there
# alone, whatever the labels, and so does the merge queue (`merge_group`); a pull request on Linux, plus macOS when it's labelled `ci:macos` and
# Windows (x64 and ARM64) when it's labelled `ci:windows` (`ci:full` asks for both). For the label
# just added (the `CI on label` workflow), only the platform that label asks for: the pull
# request's own run already covers Linux.
#
# From the event: EVENT (github.event_name), ACTION (github.event.action), LABEL (the label
# added), LABELS (the pull request's labels, as JSON) and PLATFORMS (a manual run's choice).
set -euo pipefail

# Whether the pull request asks for a platform's label (ci:full asks for every one).
asks() {
  if [ "${ACTION:-}" = labeled ]; then
    [ "${LABEL:-}" = "$1" ] || [ "${LABEL:-}" = ci:full ]
  else
    jq -e --arg l "$1" 'index($l) or index("ci:full")' <<<"${LABELS:-[]}" >/dev/null
  fi
}

os=()
[ "${ACTION:-}" = labeled ] || os+=(ubuntu-latest)
if [ "${EVENT:-}" = merge_group ] || { [ "${EVENT:-}" = workflow_dispatch ] && [ "${PLATFORMS:-all}" = linux ]; }; then
  jq -cn '$ARGS.positional' --args ${os[@]+"${os[@]}"}
  exit 0
fi
if [ "${EVENT:-}" != pull_request ] || asks ci:macos; then os+=(macos-latest); fi
# Windows on ARM64 with x64: its release build ships (#1143), so a break shows before a release.
if [ "${EVENT:-}" != pull_request ] || asks ci:windows; then os+=(windows-latest windows-11-arm); fi
jq -cn '$ARGS.positional' --args ${os[@]+"${os[@]}"}
