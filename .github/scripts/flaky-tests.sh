#!/usr/bin/env bash
# The tests a cargo-nextest run passed only on a retry, from its JUnit report (the `ci` profile in
# .config/nextest.toml): one a line, as nextest names them, "<binary> <test>". Prints nothing when
# there are none or the run wrote no report (a build that failed).
#
#   .github/scripts/flaky-tests.sh target/nextest/ci/junit.xml
set -euo pipefail

report="${1:?usage: flaky-tests.sh <junit.xml>}"
[ -f "$report" ] || exit 0

# A flaky test is a <testcase> with a <flakyFailure> or <flakyError> inside it; the failed
# attempts' output in the report is escaped, so a `<testcase` on a line is always a test's own.
awk '
  function attr(line, name,    m) {
    if (match(line, " " name "=\"[^\"]*\"") == 0) return ""
    m = substr(line, RSTART + length(name) + 3, RLENGTH - length(name) - 4)
    return m
  }
  /<testcase / { test = attr($0, "classname") " " attr($0, "name") }
  /<flaky(Failure|Error)/ { if (test != "") flaky[test] = 1 }
  END { for (t in flaky) print t }
' "$report" | sort
