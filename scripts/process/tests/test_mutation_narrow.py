import json
import os
import re
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import mutation_narrow as N

# A made-up crate: lib.rs declares bells (a dir module), gears (names bells), plain (does
# not), and a #[path] test tree whose bells_flow file names bells.
CRATE = {
    "lib.rs": "mod bells;\nmod gears;\nmod plain;\n#[cfg(test)]\n#[path = \"tests/flows/mod.rs\"]\nmod flows;\n"
              "pub fn run() { bells::start(); }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn root() {}\n}\n",
    "bells/mod.rs": "pub mod ring;\npub fn start() {}\n",
    "bells/ring.rs": "pub fn ring() -> bool { true }\n",
    "gears.rs": "use crate::bells::ring::ring;\npub fn turn() -> bool { ring() }\n",
    "plain.rs": "pub fn alone() {}\n",
    "tests/flows/mod.rs": "mod bells_flow;\nmod other_flow;\n",
    "tests/flows/bells_flow.rs": "use crate::bells;\n",
    "tests/flows/other_flow.rs": "use crate::plain;\n",
}


def write_crate(root: Path, files=CRATE) -> Path:
    src = root / "app" / "src"
    for rel, text in files.items():
        (src / rel).parent.mkdir(parents=True, exist_ok=True)
        (src / rel).write_text(text)
    return src


def scope(names, files=CRATE):
    with tempfile.TemporaryDirectory() as d:
        write_crate(Path(d), files)
        cwd = os.getcwd()
        os.chdir(d)
        try:
            return N.scope(Path("app/src"), names)
        finally:
            os.chdir(cwd)


def filter_regex(result: str) -> re.Pattern:
    assert result.startswith("narrow test(/") and result.endswith("/)"), result
    return re.compile(result[len("narrow test(/"):-2])


class Scope(unittest.TestCase):
    def test_mod_tree_follows_dirs_and_path_attributes(self):
        with tempfile.TemporaryDirectory() as d:
            tree = N.mod_tree(write_crate(Path(d)))
        self.assertEqual(tree["bells/ring.rs"], "bells::ring")
        self.assertEqual(tree["tests/flows/bells_flow.rs"], "flows::bells_flow")
        self.assertEqual(tree["lib.rs"], "")

    def test_narrows_to_the_module_and_the_files_that_name_it(self):
        rx = filter_regex(scope(["app/src/bells/ring.rs:1:25: replace ring -> bool with false"]))
        for kept in ["bells::ring::tests::t", "gears::tests::t", "flows::bells_flow::t", "tests::root"]:
            self.assertRegex(kept, rx)
        for dropped in ["plain::tests::t", "flows::other_flow::t", "bellsx::t"]:
            self.assertNotRegex(dropped, rx)

    def test_fails_open_for_a_root_file_an_outside_file_or_an_unreached_file(self):
        self.assertTrue(scope(["app/src/lib.rs:7:16: replace run with ()"]).startswith("full "))
        self.assertTrue(scope(["other/src/lib.rs:1:1: replace x with ()"]).startswith("full "))
        self.assertTrue(scope(["app/src/stray.rs:1:1: replace x with ()"]).startswith("full "))

    def test_one_unmappable_mutant_sends_the_whole_shard_to_the_full_package(self):
        got = scope(["app/src/bells/ring.rs:1:25: replace ring -> bool with false",
                     "app/src/lib.rs:7:16: replace run with ()"])
        self.assertTrue(got.startswith("full "), got)

    def test_a_mod_declaration_alone_does_not_count_as_naming_the_module(self):
        files = dict(CRATE, **{"lib.rs": "mod bells;\nmod gears;\nmod plain;\n"
                               "#[cfg(test)]\nmod tests {\n    #[test]\n    fn root() {}\n}\n"})
        self.assertNotRegex("tests::root", filter_regex(scope(["app/src/bells/ring.rs:1:25: x"], files)))


def outcome(name, summary, log):
    return {"scenario": {"Mutant": {"name": name}}, "summary": summary, "log_path": log}


def write_out(out: Path, rows):
    (out / "log").mkdir(parents=True)
    outs = [{"scenario": "Baseline", "summary": "Success", "log_path": "log/baseline.log"}]
    for name, summary, text in rows:
        log = f"log/{len(outs)}.log"
        (out / log).write_text(text)
        outs.append(outcome(name, summary, log))
    (out / "outcomes.json").write_text(json.dumps({"outcomes": outs}))


FAILED = "        FAIL [   0.010s] app_lib bells::ring::tests::rings\n"


class RetryAndMerge(unittest.TestCase):
    def test_retries_every_mutant_not_proven_caught_by_a_failing_test(self):
        with tempfile.TemporaryDirectory() as d:
            out = Path(d)
            write_out(out, [("a.rs:1:1: caught (x)", "CaughtMutant", FAILED),
                            ("a.rs:2:1: caught without a failing test", "CaughtMutant", "error: no such filter\n"),
                            ("a.rs:3:1: missed", "MissedMutant", ""),
                            ("a.rs:4:1: timeout", "Timeout", ""),
                            ("a.rs:5:1: unviable", "Unviable", "")])
            got = N.retry(out)
        self.assertEqual(len(got), 3)
        for n in ["a.rs:2:1: caught without a failing test", "a.rs:3:1: missed", "a.rs:4:1: timeout"]:
            self.assertTrue(any(re.fullmatch(r, n) for r in got), n)
        self.assertFalse(any(re.fullmatch(r, "a.rs:1:1: caught (x)") for r in got))

    def test_the_regex_matches_only_its_own_name(self):
        name = "src/a.rs:9:5: replace Pass<'_>::agent -> Vec<i64> with vec![] (a|b)"
        rx = "^" + N.rust_regex_escape(name) + "$"
        self.assertRegex(name, rx)
        self.assertNotRegex(name.replace(":9:", ":19:"), rx)

    def test_merge_takes_the_full_pass_verdict_for_every_retried_mutant(self):
        with tempfile.TemporaryDirectory() as d:
            out, full = Path(d, "p1"), Path(d, "p2")
            write_out(out, [("a.rs:1:1: kept", "CaughtMutant", FAILED),
                            ("a.rs:2:1: missed then caught", "MissedMutant", ""),
                            ("a.rs:3:1: missed both", "MissedMutant", ""),
                            ("a.rs:4:1: unproven, not retried", "CaughtMutant", "")])
            write_out(full, [("a.rs:2:1: missed then caught", "CaughtMutant", "test x ... FAILED\n"),
                             ("a.rs:3:1: missed both", "MissedMutant", "")])
            N.merge(out, full)
            read = lambda k: (out / f"{k}.txt").read_text().splitlines()
            self.assertEqual(read("caught"), ["a.rs:1:1: kept", "a.rs:2:1: missed then caught"])
            self.assertEqual(read("missed"), ["a.rs:3:1: missed both", "a.rs:4:1: unproven, not retried"])
            self.assertEqual(read("timeout"), [])


if __name__ == "__main__":
    unittest.main()
