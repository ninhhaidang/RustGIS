"""Fails a pull request that raises a limit which only goes down.

`xtask/row-uses.txt` (`cargo xtask rows`) and `xtask/memory-limits.txt` (`cargo xtask memory`) are
committed limits that AGENTS.md says only come down: the xtask checks fail when the code is over
its limit, but nothing stopped a pull request from editing the limit up to make them pass. This
check compares each limit with the one on the base commit (the pull request's base branch, or the
merge queue entry's parent) and fails when one is higher or has been removed. A new line (a new
memory scenario) is fine. Lowering a limit is what the files are for.

A limit may need to rise for a reason a maintainer accepts. A maintainer overrides the check for
one change by commenting on the pull request, or reviewing it, with a line of its own that the
failure prints for that change, such as
`<!-- ratchet-raise-approved: xtask/row-uses.txt count=790 -->`. The line names the file, the
limit and the value it was approved at, so a later push that raises the limit further, or another
limit, needs its own approval. A removed limit is approved the same
way (`... memory-limits.txt save removed -->`), as is a deleted file. A maintainer here is the
repository's owner, a member of its organization or a collaborator, as GitHub labels the message;
the pull request's author may be that maintainer: the override is a deliberate, recorded decision.

The check compares the two data files with their base. It does not look at the code that reads
them (`xtask/src/rows.rs`, `xtask/src/memory.rs`), and it runs from the pull request's own
checkout, like the other checks here, so a pull request that loosens those is for review to catch.

Only these two files are checked. The other hand-kept lists (the commands the hostile tests skip,
the known-silent reader cases, the cancellation exemptions) grow for good reasons, and AGENTS.md
doesn't say they only go down.

CI runs this on pull requests and in the merge queue. It needs BASE_SHA (the pull request's base
commit, or the queue entry's parent), GH_TOKEN (read access to pull requests, used only when a
limit has risen), GITHUB_REPOSITORY, and PR_NUMBER or, in the merge queue, MERGE_GROUP_REF.
"""

import hashlib
import json
import math
import os
import re
import subprocess
import sys

ROWS_FILE = "xtask/row-uses.txt"
LIMIT_FILES = [ROWS_FILE, "xtask/memory-limits.txt"]
APPROVE = "ratchet-raise-approved"
MAINTAINERS = {"OWNER", "MEMBER", "COLLABORATOR"}
# What Rust's `str::trim` removes (Unicode White_Space). Python's `strip` also removes U+001C to
# U+001F, so a line that starts with one would read as a different key here than in the xtask readers.
RUST_WHITESPACE = "\t\n\x0b\x0c\r \x85\xa0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000"
QUEUE_REF = re.compile(r"/pr-(\d+)-[0-9a-f]{40}$")


def parse(text, rows_file=False):
    """The `name = number` lines of a limits file, a `#` starting a comment; the first line of a name counts.

    The xtask readers take the first line of a name too, so a later one can't move the limit.
    `rows_file` reads `xtask/row-uses.txt` as `cargo xtask rows` does: only `count` matters, and a
    line without an `=` is ignored. `cargo xtask memory` rejects a line without one.
    """
    limits = {}
    # Lines end at LF or CRLF only, as in Rust's `str::lines`: `splitlines` would also split at a
    # vertical tab, form feed or bare CR, which the xtask readers keep inside the line.
    for n, raw in enumerate(text.split("\n"), 1):
        line = raw.removesuffix("\r").split("#", 1)[0].strip(RUST_WHITESPACE)
        if not line:
            continue
        name, sep, value = line.partition("=")
        name = name.strip(RUST_WHITESPACE)
        if rows_file and (not sep or name != "count"):
            continue
        if not sep:
            raise ValueError(f"line {n}: expected `name = number`")
        value = value.strip(RUST_WHITESPACE)
        try:
            number = float(value)
        except ValueError:
            raise ValueError(f"line {n}: `{value}` is not a number") from None
        if not math.isfinite(number):
            raise ValueError(f"line {n}: `{value}` is not a finite number")
        limits.setdefault(name, (number, value))
    return limits


def marker(token):
    """The line a maintainer's message carries to approve the change named by `token`."""
    return f"<!-- {APPROVE}: {token} -->"


def compare(path, base_text, head_text):
    """The raised or removed limits of one file, as (message, approval token) pairs.

    `base_text` is None when the file didn't exist on the base (nothing to compare), `head_text` None
    when the change deletes it. A token of None means nothing can approve the problem: fix the file.
    """
    if base_text is None:
        return []
    head = None
    if head_text is not None:
        try:
            head = parse(head_text, path == ROWS_FILE)
        except ValueError as error:
            return [(f"{path}: {error}", None)]
    try:
        base = parse(base_text, path == ROWS_FILE)
    except ValueError as error:
        # The limits can't be compared, so a raise could hide behind this: it needs a maintainer's say,
        # for the head as it is (its digest is in the token), not for whatever is pushed next.
        digest = hashlib.sha256((head_text or "").encode("utf-8")).hexdigest()[:12]
        return [(f"{path}: can't be read on the base ({error}), so its limits can't be compared", f"{path} unreadable on base, head {digest}")]
    if head is None:
        return [(f"{path}: deleted, but it holds {len(base)} limit(s) that only go down", f"{path} deleted")]
    problems = []
    for name, (old, old_text) in base.items():
        if name not in head:
            problems.append((f"{path}: the limit `{name}` ({old_text}) was removed", f"{path} {name} removed"))
        elif head[name][0] > old:
            problems.append((f"{path}: `{name}` rose from {old_text} to {head[name][1]}", f"{path} {name}={head[name][1]}"))
    return problems


def run(args):
    """Runs a command and returns its standard output as it is, line endings included; raises if it fails."""
    return subprocess.run(args, check=True, stdout=subprocess.PIPE, timeout=120).stdout.decode("utf-8", "replace")


def base_mode(base_sha, path):
    """The file's git mode on the base commit (`120000` is a symbolic link), or None when it has no such file."""
    listing = run(["git", "ls-tree", base_sha, "--", path]).split()
    return listing[0] if listing else None


def base_file(base_sha, path):
    """The file's text on the base commit, or None when the base has no such file."""
    if base_mode(base_sha, path) is None:
        return None
    return run(["git", "show", f"{base_sha}:{path}"])


def is_link(path):
    """Whether the checkout has a symbolic link at the path."""
    return os.path.islink(path)


def head_file(path):
    """The file's text in the checkout, with its line endings, or None when it isn't there."""
    try:
        with open(path, encoding="utf-8", newline="") as f:
            return f.read()
    except FileNotFoundError:
        return None


def objects(text):
    """Every JSON value in `text`, whether `gh` prints them one to a line or indented."""
    decoder, at, found = json.JSONDecoder(), 0, []
    while True:
        while at < len(text) and text[at].isspace():
            at += 1
        if at >= len(text):
            return found
        value, at = decoder.raw_decode(text, at)
        found.append(value)


def messages(repo, number):
    """Every comment and review on the pull request, as (body, author association, is a bot)."""
    found = []
    for endpoint in (f"repos/{repo}/issues/{number}/comments", f"repos/{repo}/pulls/{number}/reviews"):
        out = run(
            ["gh", "api", "--paginate", endpoint, "--jq",
             ".[] | {body, association: .author_association, bot: (.user.type == \"Bot\")}"]
        )
        found += objects(out)
    return found


def approved(found, token):
    """Whether a maintainer's message has the approval line for `token` on a line of its own."""
    line = marker(token)
    return any(
        not m.get("bot")
        and m.get("association") in MAINTAINERS
        and any(row.strip() == line for row in (m.get("body") or "").split("\n"))
        for m in found
    )


def pr_number():
    """The pull request to check, from PR_NUMBER or the merge queue's branch name."""
    if os.environ.get("PR_NUMBER"):
        return int(os.environ["PR_NUMBER"])
    match = QUEUE_REF.search(os.environ.get("MERGE_GROUP_REF", ""))
    if match is None:
        raise SystemExit("::error::No PR_NUMBER, and MERGE_GROUP_REF names no pull request.")
    return int(match.group(1))


def escape(text):
    """Text for a workflow command line: a newline must not start a command of its own."""
    return text.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def main():
    """Checks the limits against the base and exits 1 if one rose or was removed without a maintainer's say."""
    base_sha = os.environ.get("BASE_SHA", "")
    if not re.fullmatch(r"[0-9a-f]{40}", base_sha):
        raise SystemExit("::error::BASE_SHA is not a commit hash, so there is nothing to compare the limits with.")
    run(["git", "fetch", "--no-tags", "--depth=1", "origin", base_sha])
    problems = []
    for path in LIMIT_FILES:
        # A link reads as its target here and as its own text in git, so the two sides would not
        # be the same file: neither is accepted.
        if is_link(path) or base_mode(base_sha, path) == "120000":
            problems.append((f"{path}: is a symbolic link, but the limits must be in a regular file", None))
            continue
        problems += compare(path, base_file(base_sha, path), head_file(path))
    if not problems:
        print("No limit that only goes down has risen.")
        return 0
    number = pr_number()
    found = messages(os.environ["GITHUB_REPOSITORY"], number)
    refused = []
    for message, token in problems:
        if token is not None and approved(found, token):
            print(f"::notice::{escape(message)}: approved by a maintainer on #{number}")
        else:
            refused.append((message, token))
            print(f"::error::{escape(message)}")
    if not refused:
        return 0
    print(
        f"#{number} raises {len(refused)} limit(s) that only go down. Fix the code so the old limit holds, "
        "or, if the change is deliberate, a maintainer comments on the pull request with each of these "
        "lines on a line of its own, and the check is run again (re-run the job, or queue the pull request "
        "again). A removed memory scenario needs its line too."
    )
    for _, token in refused:
        if token is not None:
            print(marker(token))
    return 1


if __name__ == "__main__":
    sys.exit(main())
