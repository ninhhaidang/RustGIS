"""Checks for ratchet-limits.py on limits files cut down from xtask/row-uses.txt and xtask/memory-limits.txt."""

import importlib.util
import json
from pathlib import Path
import unittest
import unittest.mock

SPEC = importlib.util.spec_from_file_location("ratchet_limits", Path(__file__).with_name("ratchet-limits.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

ROWS = "# Direct uses = rows outside the data crate.\ncount = 782\n"
MEMORY = "# ratios\nload_mib = 200.00\njoin = 2.10  # a note\nedits_growth = 0.10\n"
SHA = "a" * 40


def person(body, association="MEMBER", bot=False):
    return {"body": body, "association": association, "bot": bot}


class RatchetLimits(unittest.TestCase):
    def problems(self, base, head):
        return [message for message, _ in MODULE.compare("xtask/x.txt", base, head)]

    def tokens(self, base, head):
        return [token for _, token in MODULE.compare("xtask/x.txt", base, head)]

    def test_a_raised_row_count_is_refused(self):
        found = self.problems(ROWS, ROWS.replace("782", "783"))
        self.assertEqual(len(found), 1)
        self.assertIn("`count` rose from 782 to 783", found[0])

    def test_an_unchanged_or_lowered_limit_passes(self):
        self.assertEqual(self.problems(ROWS, ROWS), [])
        self.assertEqual(self.problems(ROWS, ROWS.replace("782", "700")), [])
        self.assertEqual(self.problems(MEMORY, MEMORY.replace("2.10", "1.9")), [])

    def test_each_memory_limit_is_held_to_its_own_value(self):
        head = MEMORY.replace("join = 2.10", "join = 2.11").replace("200.00", "150")
        found = self.problems(MEMORY, head)
        self.assertEqual(len(found), 1)
        self.assertIn("`join` rose from 2.10 to 2.11", found[0])

    def test_a_decimal_that_reads_the_same_is_not_a_raise(self):
        self.assertEqual(self.problems(MEMORY, MEMORY.replace("200.00", "200.0")), [])

    def test_a_removed_limit_is_refused_but_a_new_one_is_fine(self):
        found = self.problems(MEMORY, MEMORY.replace("edits_growth = 0.10\n", ""))
        self.assertEqual(len(found), 1)
        self.assertIn("`edits_growth` (0.10) was removed", found[0])
        self.assertEqual(self.problems(MEMORY, MEMORY + "pdf = 3.0\n"), [])

    def test_commenting_a_limit_out_removes_it(self):
        found = self.problems(MEMORY, MEMORY.replace("join = 2.10", "# join = 2.10"))
        self.assertEqual(len(found), 1)

    def test_a_deleted_file_is_refused_and_a_new_one_is_fine(self):
        self.assertEqual(len(self.problems(ROWS, None)), 1)
        self.assertEqual(self.problems(None, ROWS), [])

    def test_the_first_line_of_a_name_is_the_limit_as_xtask_reads_it(self):
        self.assertEqual(MODULE.parse("count = 5\ncount = 9\n")["count"][0], 5)
        self.assertEqual(len(self.problems(ROWS, "count = 782\ncount = 999\n")), 0)
        self.assertEqual(len(self.problems(ROWS, "count = 999\ncount = 782\n")), 1)

    def test_a_file_that_cannot_be_read_fails_on_the_head_and_is_skipped_on_the_base(self):
        found = self.problems(ROWS, "count = many\n")
        self.assertEqual(len(found), 1)
        self.assertIn("not a number", found[0])
        self.assertEqual(len(self.problems(ROWS, "count\n")), 1)
        self.assertEqual(len(self.problems(ROWS, "count = nan\n")), 1)
        # An unreadable base can't be compared, so it needs a maintainer's say, not a pass.
        self.assertEqual(len(self.tokens("count = many\n", ROWS)), 1)
        # The approval is for the head it was given on: another head needs another line.
        [first] = self.tokens("count = many\n", ROWS)
        [second] = self.tokens("count = many\n", ROWS.replace("782", "900"))
        self.assertTrue(first.startswith("xtask/x.txt unreadable on base, head ") and first != second)
        self.assertEqual(self.tokens("count = many\n", ROWS), [first])
        # Nothing approves a head that doesn't parse, whatever the base is.
        self.assertEqual(self.tokens("count = many\n", "count = oops\n"), [None])

    def test_the_row_count_is_read_as_cargo_xtask_rows_reads_it(self):
        path = MODULE.ROWS_FILE
        note = "see AGENTS.md\nsome = thing\n" + ROWS
        # A line without an `=`, and a key other than `count`, are ignored there, so they are here.
        self.assertEqual(MODULE.compare(path, note, note), [])
        self.assertEqual(MODULE.compare(path, note, ROWS), [])
        found = MODULE.compare(path, note, ROWS.replace("782", "900"))
        self.assertEqual([message for message, _ in found], [f"{path}: `count` rose from 782 to 900"])
        # `cargo xtask memory` rejects such a line, and so does the check on that file.
        found = MODULE.compare(MODULE.LIMIT_FILES[1], "see AGENTS.md\n" + MEMORY, MEMORY)
        self.assertEqual(len(found), 1)
        self.assertIn("unreadable on base", found[0][1])

    def test_messages_reads_comments_and_reviews_one_object_per_line(self):
        asked = []

        def run(args):
            asked.append(args[3])
            return '{"body": "a", "association": "MEMBER", "bot": false}\n\n{"body": "b", "association": "NONE", "bot": true}\n'

        with unittest.mock.patch.object(MODULE, "run", run):
            found = MODULE.messages("o/r", 7)
        self.assertEqual(asked, ["repos/o/r/issues/7/comments", "repos/o/r/pulls/7/reviews"])
        self.assertEqual([m["body"] for m in found], ["a", "b", "a", "b"])

    def test_gh_output_may_be_compact_or_indented(self):
        compact = '{"body": "a", "association": "MEMBER", "bot": false}\n{"body": "b", "association": "NONE", "bot": true}\n'
        indented = json.dumps(MODULE.objects(compact), indent=2)[1:-1].replace("},", "}\n")
        self.assertEqual(MODULE.objects(compact), MODULE.objects(indented))
        self.assertEqual(len(MODULE.objects(indented)), 2)
        self.assertEqual(MODULE.objects(""), [])
        self.assertEqual(MODULE.objects("  \n"), [])

    def test_the_pull_request_number_comes_from_the_environment_first(self):
        ref = "refs/heads/gh-readonly-queue/main/pr-2655-76a6cd2823b4431c363e30a8023c7f69bccae938"
        with unittest.mock.patch.dict(MODULE.os.environ, {"PR_NUMBER": "12", "MERGE_GROUP_REF": ref}):
            self.assertEqual(MODULE.pr_number(), 12)
        with unittest.mock.patch.dict(MODULE.os.environ, {"PR_NUMBER": "", "MERGE_GROUP_REF": "refs/heads/main"}):
            with self.assertRaises(SystemExit):
                MODULE.pr_number()

    def test_each_problem_has_its_own_approval_token(self):
        self.assertEqual(self.tokens(ROWS, ROWS.replace("782", "783")), ["xtask/x.txt count=783"])
        self.assertEqual(self.tokens(MEMORY, MEMORY.replace("join = 2.10", "join = 2.2")), ["xtask/x.txt join=2.2"])
        self.assertEqual(self.tokens(MEMORY, MEMORY.replace("edits_growth = 0.10\n", "")), ["xtask/x.txt edits_growth removed"])
        self.assertEqual(self.tokens(ROWS, None), ["xtask/x.txt deleted"])
        self.assertEqual(self.tokens(ROWS, "count = many\n"), [None])

    def test_only_a_maintainers_line_for_that_change_approves(self):
        line = MODULE.marker("xtask/x.txt count=790")
        self.assertEqual(line, "<!-- ratchet-raise-approved: xtask/x.txt count=790 -->")
        self.assertTrue(MODULE.approved([person(f"Needed for the new loader.\n{line}\n")], "xtask/x.txt count=790"))
        self.assertTrue(MODULE.approved([person(f"x\n  {line}  \nthanks", "OWNER")], "xtask/x.txt count=790"))
        self.assertFalse(MODULE.approved([person(line, "CONTRIBUTOR")], "xtask/x.txt count=790"))
        self.assertFalse(MODULE.approved([person(line, "NONE")], "xtask/x.txt count=790"))
        self.assertFalse(MODULE.approved([person(line, bot=True)], "xtask/x.txt count=790"))
        self.assertFalse(MODULE.approved([person(f"I won't reply `{line}` to this.")], "xtask/x.txt count=790"))
        self.assertFalse(MODULE.approved([person(f"> {line}")], "xtask/x.txt count=790"))
        self.assertFalse(MODULE.approved([person(None)], "xtask/x.txt count=790"))
        self.assertFalse(MODULE.approved([], "xtask/x.txt count=790"))

    def test_an_approval_is_for_the_value_and_the_limit_it_names(self):
        line = person(MODULE.marker("xtask/x.txt count=790"))
        self.assertFalse(MODULE.approved([line], "xtask/x.txt count=900"))
        self.assertFalse(MODULE.approved([line], "xtask/y.txt count=790"))
        self.assertFalse(MODULE.approved([line], "xtask/x.txt join=790"))
        self.assertFalse(MODULE.approved([line], "xtask/x.txt count removed"))

    def test_the_merge_queue_branch_names_the_pull_request(self):
        ref = "refs/heads/gh-readonly-queue/main/pr-2655-76a6cd2823b4431c363e30a8023c7f69bccae938"
        with unittest.mock.patch.dict(MODULE.os.environ, {"PR_NUMBER": "", "MERGE_GROUP_REF": ref}):
            self.assertEqual(MODULE.pr_number(), 2655)

    def main(self, base, head, messages, link_in_head=False, link_on_base=False):
        env = {"BASE_SHA": SHA, "GITHUB_REPOSITORY": "o/r", "PR_NUMBER": "7"}
        files = dict(zip(MODULE.LIMIT_FILES, head))
        asked = []

        def messages_of(repo, number):
            asked.append((repo, number))
            return messages

        with (
            unittest.mock.patch.dict(MODULE.os.environ, env),
            unittest.mock.patch.object(MODULE, "run", lambda args: ""),
            unittest.mock.patch.object(MODULE, "base_mode", lambda sha, path: link_on_base and "120000" or "100644"),
            unittest.mock.patch.object(MODULE, "is_link", lambda path: link_in_head),
            unittest.mock.patch.object(MODULE, "base_file", lambda sha, path: dict(zip(MODULE.LIMIT_FILES, base))[path]),
            unittest.mock.patch.object(MODULE, "head_file", lambda path: files[path]),
            unittest.mock.patch.object(MODULE, "messages", messages_of),
            unittest.mock.patch("builtins.print"),
        ):
            return MODULE.main(), asked

    def test_main_fails_on_a_raise_and_passes_with_an_approval_of_that_value(self):
        raised = (ROWS.replace("782", "900"), MEMORY)
        self.assertEqual(self.main((ROWS, MEMORY), raised, []), (1, [("o/r", 7)]))
        row_path = MODULE.LIMIT_FILES[0]
        ok = person(MODULE.marker(f"{row_path} count=900"))
        self.assertEqual(self.main((ROWS, MEMORY), raised, [ok])[0], 0)
        stale = person(MODULE.marker(f"{row_path} count=790"))
        self.assertEqual(self.main((ROWS, MEMORY), raised, [stale])[0], 1)

    def test_main_needs_an_approval_for_every_change(self):
        row_path, memory_path = MODULE.LIMIT_FILES
        raised = (ROWS.replace("782", "900"), MEMORY.replace("join = 2.10", "join = 3").replace("edits_growth = 0.10\n", ""))
        one = person(MODULE.marker(f"{row_path} count=900"))
        both = person(f"{MODULE.marker(f'{memory_path} join=3')}\n{MODULE.marker(f'{memory_path} edits_growth removed')}")
        self.assertEqual(self.main((ROWS, MEMORY), raised, [one])[0], 1)
        self.assertEqual(self.main((ROWS, MEMORY), raised, [one, both])[0], 0)

    def test_a_symbolic_link_at_a_limit_path_is_never_accepted(self):
        approving = [person(MODULE.marker(f"{path} {name}")) for path in MODULE.LIMIT_FILES for name in ("None", "count=782")]
        self.assertEqual(self.main((ROWS, MEMORY), (ROWS, MEMORY), approving, link_in_head=True)[0], 1)
        self.assertEqual(self.main((ROWS, MEMORY), (ROWS, MEMORY), approving, link_on_base=True)[0], 1)
        self.assertEqual(self.main((ROWS, MEMORY), (ROWS, MEMORY), [])[0], 0)

    def test_lines_end_at_lf_or_crlf_only_as_the_xtask_readers_have_them(self):
        # Rust keeps a vertical tab, form feed or bare CR inside a line: here the comment hides the first count.
        for separator in ("\v", "\f", "\r", "\x1c", "\u2028"):
            head = f"# note{separator}count = 782\ncount = 999\n"
            self.assertEqual(len(self.problems(ROWS, head)), 1, repr(separator))
        self.assertEqual(MODULE.parse("count = 5\r\ncount = 9\r\n")["count"][0], 5)
        # Python's `strip` removes U+001C to U+001F; Rust's `trim` doesn't, so the xtask readers
        # take the second line's 999 as the limit.
        for control in ("\x1c", "\x1d", "\x1e", "\x1f"):
            found = self.problems(ROWS, f"{control}count = 782\ncount = 999\n")
            self.assertEqual(len(found), 1, repr(control))
            self.assertIn("rose from 782 to 999", found[0])
        self.assertEqual(MODULE.parse("\u3000count\u00a0=\u2003 5 \n")["count"][0], 5)
        self.assertEqual(self.problems(ROWS, ROWS.replace("\n", "\r\n")), [])

    def test_main_never_lets_an_unreadable_file_through(self):
        row_path = MODULE.LIMIT_FILES[0]
        approving = [person(MODULE.marker(f"{row_path} None")), person(MODULE.marker(f"{row_path} count=many"))]
        self.assertEqual(self.main((ROWS, MEMORY), ("count = many\n", MEMORY), approving)[0], 1)

    def test_main_asks_nobody_when_no_limit_rose(self):
        self.assertEqual(self.main((ROWS, MEMORY), (ROWS.replace("782", "1"), MEMORY), []), (0, []))

    def test_main_refuses_a_base_that_is_not_a_commit(self):
        with unittest.mock.patch.dict(MODULE.os.environ, {"BASE_SHA": ""}):
            with self.assertRaises(SystemExit):
                MODULE.main()


if __name__ == "__main__":
    unittest.main()
