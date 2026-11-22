"""Checks for review-deferrals.py on review threads recorded from pull requests, cut down to the fields it reads."""

import importlib.util
from pathlib import Path
import unittest
import unittest.mock

SPEC = importlib.util.spec_from_file_location("review_deferrals", Path(__file__).with_name("review-deferrals.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

AUTHOR = "contributor"
REVIEWER = {"login": "github-actions", "__typename": "Bot"}
CODERABBIT = {"login": "coderabbitai", "__typename": "Bot"}


def comment(author, body, association="CONTRIBUTOR", editor=None):
    return {
        "url": f"https://github.com/o/r/pull/1#discussion_r{abs(hash(body)) % 10**9}",
        "body": body,
        "authorAssociation": association,
        "lastEditedAt": "2026-11-16T03:39:59Z" if editor else None,
        "author": author,
        "editor": {"login": editor} if editor else None,
    }


def thread(*comments, path="crates/data/src/dbf.rs"):
    return {"path": path, "line": 120, "comments": list(comments)}


def finding(text, kind=None, editor=None):
    marker = f"\n\nCategory: {kind}" if kind else ""
    return comment(REVIEWER, text + marker, editor=editor)


def reply(text, author=AUTHOR, association="MEMBER"):
    return comment({"login": author, "__typename": "User"}, text, association)


DEFER = reply("The reader skipping deleted records predates this PR; left for the release quality pass. <!-- quality-sweep -->")
BUG = "**Bug:** a deleted record shifts every later row's attributes by one."


class ReviewDeferrals(unittest.TestCase):
    def check(self, *threads):
        return MODULE.check(list(threads))

    def test_a_deferred_bug_is_refused(self):
        refused, unclassified = self.check(thread(finding(BUG, "bug"), DEFER))
        self.assertEqual(len(refused), 1)
        self.assertIn("classed it as bug", refused[0])
        self.assertEqual(unclassified, [])

    def test_a_deferred_quality_point_passes(self):
        quality = finding("**Quality:** appending `dbf` keeps the fuzz corpus's extension indexes.", "quality")
        self.assertEqual(self.check(thread(quality, DEFER)), ([], []))

    def test_an_undeferred_bug_is_not_this_checks_business(self):
        answer = reply("Fixed in d3eda9c78.")
        self.assertEqual(self.check(thread(finding(BUG, "bug"), answer)), ([], []))

    def test_a_grouped_comment_takes_its_most_serious_category(self):
        both = finding(BUG + "\nCategory: quality", "agents-md")
        refused, _ = self.check(thread(both, DEFER))
        self.assertIn("classed it as agents-md", refused[0])

    def test_only_a_maintainer_overrides_the_check(self):
        rule = finding("**Never silent:** the deleted-record notice has no test.", "agents-md")
        by_contributor = reply("Misclassified. <!-- deferral-approved -->", association="CONTRIBUTOR")
        by_bot = comment(CODERABBIT, "<!-- deferral-approved -->", "MEMBER")
        by_maintainer_author = reply("An extra test for working code. <!-- deferral-approved -->")
        mention = reply("I won't reply `<!-- deferral-approved -->` to this one.")
        self.assertEqual(len(self.check(thread(rule, DEFER, by_contributor, by_bot, mention))[0]), 1)
        self.assertEqual(self.check(thread(rule, DEFER, by_maintainer_author)), ([], []))

    def test_an_edited_finding_counts_as_must_fix(self):
        edited = finding(BUG, "quality", editor=AUTHOR)
        refused, _ = self.check(thread(edited, DEFER))
        self.assertIn("edited after the review posted it", refused[0])

    def test_threads_without_a_category_pass_with_a_notice(self):
        rabbit = thread(comment(CODERABBIT, "_Potential issue_ | **Major** Record the uppercase DBF sidecar."), DEFER)
        person = thread(reply("Should this clamp?", author="maintainer"), DEFER)
        old_review = thread(finding("**Bug risk (low):** every non-I/O record error is counted."), DEFER)
        unknown = thread(finding(BUG, "bugs"), DEFER)
        refused, unclassified = self.check(rabbit, person, old_review, unknown)
        self.assertEqual(refused, [])
        self.assertEqual(len(unclassified), 4)

    def test_the_marker_counts_wherever_it_sits_as_the_sweep_collects_it(self):
        trailing = reply("Left for the release quality pass. <!-- quality-sweep --> Will do after the merge.")
        tracked = reply("Left for the release quality pass. <!-- quality-sweep -->\n<!-- followup:inline:42 -->\n")
        for deferral in (trailing, tracked):
            self.assertEqual(len(self.check(thread(finding(BUG, "bug"), deferral))[0]), 1)

    def test_a_category_line_may_be_emphasised_or_capitalised(self):
        for line in ("**Category:** bug", "Category: `bug`", "Category: Bug", "_Category_: BUG", "Category: bug.", "Category: bug\r"):
            refused, _ = self.check(thread(comment(REVIEWER, BUG + "\n\n" + line), DEFER))
            self.assertEqual(len(refused), 1, line)

    def test_an_edited_finding_without_a_category_is_refused_too(self):
        refused, unclassified = self.check(thread(finding(BUG, editor=AUTHOR), DEFER))
        self.assertEqual((len(refused), unclassified), (1, []))

    def test_a_category_is_a_line_of_its_own(self):
        prose = finding("The Category: bug label in the legend is cut short.\n\nCategory: quality")
        self.assertEqual(self.check(thread(prose, DEFER)), ([], []))

    def test_a_marker_in_the_opening_comment_is_not_a_deferral(self):
        self.assertEqual(self.check(thread(finding(BUG + " <!-- quality-sweep -->", "bug"))), ([], []))

    def test_the_merge_queue_branch_names_the_pull_request(self):
        ref = "refs/heads/gh-readonly-queue/main/pr-2655-76a6cd2823b4431c363e30a8023c7f69bccae938"
        with unittest.mock.patch.dict(MODULE.os.environ, {"PR_NUMBER": "", "MERGE_GROUP_REF": ref}):
            self.assertEqual(MODULE.pr_number(), 2655)

    def test_fetch_reads_every_page_of_threads_and_comments(self):
        def page(nodes, cursor=None):
            return {"pageInfo": {"hasNextPage": cursor is not None, "endCursor": cursor}, "nodes": nodes}

        first, second, third = finding(BUG, "bug"), reply("a"), DEFER
        answers = iter([
            {"repository": {"pullRequest": {"reviewThreads": page(
                [{"id": "T1", "path": "a.rs", "line": 1, "comments": page([first], "c1")}], "t1")}}},
            {"node": {"comments": page([second], "c2")}},
            {"node": {"comments": page([third])}},
            {"repository": {"pullRequest": {"reviewThreads": page(
                [{"id": "T2", "path": "b.rs", "line": 2, "comments": page([])}])}}},
        ])
        with unittest.mock.patch.object(MODULE, "graphql", lambda *a, **k: next(answers)):
            threads = MODULE.fetch("o/r", 1)
        self.assertEqual([t["id"] for t in threads], ["T1", "T2"])
        self.assertEqual(threads[0]["comments"], [first, second, third])


if __name__ == "__main__":
    unittest.main()
