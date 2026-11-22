"""Fails a pull request that leaves a defect for the release quality pass.

A review point may wait for the release quality pass when the reply to it ends with
`<!-- quality-sweep -->`, but only a quality point may (AGENTS.md, "Review comments"). The Claude
review (claude-code-review.yml, posting as github-actions) ends each inline finding with its
category, a last line `Category: bug` and so on, so the reviewer classifies a finding and the
author doesn't. This check refuses a deferral reply in a thread that the reviewer opened with a
`bug`, `security` or `agents-md` finding, or whose opening comment someone else has since edited.

Threads opened by anyone else (CodeRabbit, people) or by a review from before categories carry no
category and are let through: a reviewer there can answer the reply. They are listed in a notice.
An edited review finding is refused whatever category it now shows, since the edit may have
removed or changed it; the override below settles a harmless edit.

`performance` is left out on purpose: a faster path may wait. A performance finding that is also a
crash path or breaks "Size for real data" carries `bug` or `agents-md`, the more serious category.

Both markers err towards refusing. The deferral marker counts wherever it sits in a reply, as the
release quality pass collects it, so a reply that only mentions it is refused too: reword it. The
override counts only when it ends its reply, so a mention doesn't approve.

A maintainer overrides the check for a finding, misclassified or accepted as it is, by replying in
its thread with `<!-- deferral-approved -->`. A maintainer here is the repository's owner, a member
of its organization or a collaborator, as GitHub labels the reply. The author may be that maintainer: the
override is a deliberate, recorded decision, not a second opinion.

CI runs this on each push and again in the merge queue, which sees replies posted after the last
push. It needs GH_TOKEN (read access to pull requests), GITHUB_REPOSITORY, and PR_NUMBER or, in the
merge queue, MERGE_GROUP_REF.
"""

import json
import os
import re
import subprocess
import sys

DEFER = "<!-- quality-sweep -->"
APPROVE = "<!-- deferral-approved -->"
# Visible text on a line of its own: the review's inline-comment tool strips hidden comments.
# Markdown emphasis and code marks around the label or the value, and capitals, are let through.
CATEGORY = re.compile(r"^[*_`]*Category[*_`]*:[*_` \t]*([A-Za-z-]+)[*_`]*\.?[ \t\r]*$", re.M)
MUST_FIX = {"bug", "security", "agents-md"}
MAY_WAIT = {"performance", "quality"}
REVIEWER = "github-actions"
MAINTAINERS = {"OWNER", "MEMBER", "COLLABORATOR"}
QUEUE_REF = re.compile(r"/pr-(\d+)-[0-9a-f]{40}$")

THREADS = """
query($owner: String!, $name: String!, $number: Int!, $endCursor: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 50, after: $endCursor) {
        pageInfo { hasNextPage endCursor }
        nodes { id path line originalLine comments(first: 100) { %s } }
      }
    }
  }
}
"""
COMMENTS = """pageInfo { hasNextPage endCursor }
  nodes { url body authorAssociation lastEditedAt author { login __typename } editor { login } }"""
MORE_COMMENTS = """
query($id: ID!, $endCursor: String) {
  node(id: $id) { ... on PullRequestReviewThread { comments(first: 100, after: $endCursor) { %s } } }
}
"""


def is_reviewer(comment):
    """Whether a comment was posted by the Claude review workflow's bot account."""
    author = comment.get("author") or {}
    return author.get("login") == REVIEWER and author.get("__typename") == "Bot"


def category(opening):
    """The category a reviewer finding gives itself: the most serious of its markers, or None."""
    if not is_reviewer(opening):
        return None
    found = [c.lower() for c in CATEGORY.findall(opening.get("body") or "")]
    found = [c for c in found if c in MUST_FIX | MAY_WAIT]  # an unknown word is no category
    serious = [c for c in found if c in MUST_FIX]
    return (serious or found or [None])[0]


def edited_by_someone_else(opening):
    """Whether a human or other account edited the reviewer's comment, which may have changed its category."""
    if not opening.get("lastEditedAt"):
        return False
    return ((opening.get("editor") or {}).get("login")) != REVIEWER


def approved(replies):
    """Whether a maintainer overrode the check for the thread's deferral."""
    for reply in replies:
        author = reply.get("author") or {}
        if (
            (reply.get("body") or "").rstrip().endswith(APPROVE)
            and author.get("__typename") != "Bot"
            and reply.get("authorAssociation") in MAINTAINERS
        ):
            return True
    return False


def check(threads):
    """Sorts the threads that hold a deferral reply into (refused, unclassified) lists of messages."""
    refused, unclassified = [], []
    for thread in threads:
        comments = thread.get("comments") or []
        if not comments:
            continue
        opening, replies = comments[0], comments[1:]
        deferral = next((r for r in replies if DEFER in (r.get("body") or "")), None)
        if deferral is None:
            continue
        where = f"{thread.get('path')}:{thread.get('line') or thread.get('originalLine') or '?'}"
        if not is_reviewer(opening):
            unclassified.append(f"{where} ({(opening.get('author') or {}).get('login', 'ghost')}): {deferral.get('url')}")
            continue
        kind = category(opening)
        if edited_by_someone_else(opening):
            reason = "its finding was edited after the review posted it"
        elif kind in MUST_FIX:
            reason = f"the reviewer classed it as {kind}"
        else:
            if kind is None:
                unclassified.append(f"{where} (review without a category): {deferral.get('url')}")
            continue
        if not approved(replies):
            refused.append(f"{where}: left for the release quality pass, but {reason}. {deferral.get('url')}")
    return refused, unclassified


def graphql(query, **variables):
    """Runs one GraphQL query through the gh CLI and returns its data."""
    args = ["gh", "api", "graphql", "-f", f"query={query}"]
    for key, value in variables.items():
        args += ["-F" if isinstance(value, int) else "-f", f"{key}={value}"]
    out = subprocess.run(args, check=True, stdout=subprocess.PIPE, text=True, timeout=120).stdout
    return json.loads(out)["data"]


def fetch(repo, number):
    """The pull request's review threads, every comment of each in order."""
    owner, name = repo.split("/", 1)
    threads, cursor = [], None
    while True:
        extra = {"endCursor": cursor} if cursor else {}
        pr = graphql(THREADS % COMMENTS, owner=owner, name=name, number=number, **extra)["repository"]["pullRequest"]
        page = pr["reviewThreads"]
        for node in page["nodes"]:
            comments, more = node["comments"]["nodes"], node["comments"]["pageInfo"]
            while more["hasNextPage"]:
                rest = graphql(MORE_COMMENTS % COMMENTS, id=node["id"], endCursor=more["endCursor"])["node"]["comments"]
                comments, more = comments + rest["nodes"], rest["pageInfo"]
            threads.append({**node, "comments": comments})
        if not page["pageInfo"]["hasNextPage"]:
            return threads
        cursor = page["pageInfo"]["endCursor"]


def pr_number():
    """The pull request to check, from PR_NUMBER or the merge queue's branch name."""
    if os.environ.get("PR_NUMBER"):
        return int(os.environ["PR_NUMBER"])
    match = QUEUE_REF.search(os.environ.get("MERGE_GROUP_REF", ""))
    if match is None:
        raise SystemExit("::error::No PR_NUMBER, and MERGE_GROUP_REF names no pull request.")
    return int(match.group(1))


def escape(text):
    """Text for a workflow command line: a path with a newline must not start a command of its own."""
    return text.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def main():
    """Checks the pull request and exits 1 if it defers a finding that must be fixed."""
    number = pr_number()
    refused, unclassified = check(fetch(os.environ["GITHUB_REPOSITORY"], number))
    for line in unclassified:
        print(f"::notice::Deferred without a review category, not checked: {escape(line)}")
    for line in refused:
        print(f"::error::{escape(line)}")
    if refused:
        print(
            f"#{number} leaves {len(refused)} finding(s) that must be fixed for the release quality pass. "
            "Fix each in the pull request and take the marker out of the reply; for a defect too large "
            "for it, link a filed issue in the reply instead of the marker. To let one wait all the same, "
            "a maintainer replies in its thread with <!-- deferral-approved -->. "
            "Then run the checks again (re-run the job, or queue the pull request again)."
        )
        return 1
    print(f"#{number}: no deferral to the release quality pass is on a finding that must be fixed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
