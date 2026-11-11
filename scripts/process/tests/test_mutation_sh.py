"""mutation.sh rust's orchestration, driven with a stub cargo-mutants and stub build tools.

The stub acts as cargo-mutants 27.1.0 does for what the script reads: --list prints the
shard's names, a run writes mutants.out/outcomes.json (none when there are no mutants) and
exits 0 all caught, 2 missed, 3 timed out, 4 baseline failed. No cargo runs.
"""
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

MUTATION_SH = Path(__file__).resolve().parents[1] / "mutation.sh"
GATE_MUTATION = Path(__file__).resolve().parents[3] / ".github" / "workflows" / "gate-mutation.yml"

STUB = r'''#!/usr/bin/env python3
import json, os, re, sys
from pathlib import Path
cfg = json.loads(Path(os.environ["STUB_CFG"]).read_text())
args = sys.argv[1:]
with open(os.environ["STUB_LOG"], "a") as f:
    f.write(" ".join(args) + "\n")
with open(os.environ["STUB_LOG"] + ".color", "a") as f:
    f.write(os.environ.get("CARGO_TERM_COLOR", "unset") + "\n")
if "--version" in args:
    print("cargo-mutants 27.1.0"); sys.exit(0)
names = cfg["shard"] if "--shard" in args else cfg["all"]
res = [args[i + 1] for i, a in enumerate(args) if a == "--re"]
if res:
    names = [n for n in cfg["all"] if any(re.search(r, n) for r in res)]
if "--list" in args:
    if "--shard" in args and cfg.get("list_rc"):
        print("error: listing failed", file=sys.stderr); sys.exit(cfg["list_rc"])
    if "--json" in args:
        print(json.dumps([{"package": "app", "name": n} for n in names]))
    else:
        print("".join(n + "\n" for n in names), end="")
    sys.exit(0)
full = "--output" in args
out = Path("mutants.full/mutants.out" if full else "mutants.out")
out.mkdir(parents=True, exist_ok=True)
skip = "--baseline" in args and args[args.index("--baseline") + 1] == "skip"
red = cfg.get("red_tree", False)
if red and not skip:
    (out / "outcomes.json").write_text(json.dumps({"outcomes": [{"scenario": "Baseline", "summary": "Failure"}]}))
    sys.exit(4)
if not names:
    sys.exit(0)
verdict = cfg["pass2" if full else "pass1"]
outs = []
for i, n in enumerate(names):
    v = "CaughtMutant" if red else verdict.get(n, "CaughtMutant")
    log = "    FAIL [   0.1s] app tests::t\n" if v == "CaughtMutant" else ""
    # nextest's slow-timeout terminate-after killed a hung test: nextest exits non-zero, so
    # cargo-mutants 27.1.0 records CaughtMutant, and the log shows only a TIMEOUT line.
    if v == "NextestTimeout":
        v, log = "CaughtMutant", "    TIMEOUT [   5.0s] app tests::t\n"
    (out / "log").mkdir(exist_ok=True)
    (out / "log" / f"{i}.log").write_text(log)
    outs.append({"scenario": {"Mutant": {"name": n}}, "summary": v, "log_path": f"log/{i}.log"})
(out / "outcomes.json").write_text(json.dumps({"outcomes": outs}))
summaries = {o["summary"] for o in outs}
sys.exit(2 if "MissedMutant" in summaries else 3 if "Timeout" in summaries else 0)
'''

# curl writes its -o file; the rest only need to succeed. cargo is the warm build: it makes
# target/ of STUB_TARGET_MB (du reads target/.mb). df reports STUB_FREE_MB free.
TOOLS = {
    "curl": 'while [ $# -gt 0 ]; do [ "$1" = -o ] && : > "$2"; shift; done',
    "sha256sum": "cat > /dev/null",
    "tar": ":",
    "cargo": 'echo "cargo $*" >> "$STUB_LOG"; mkdir -p target; echo "$STUB_TARGET_MB" > target/.mb',
    "nproc": "echo 4",
    "du": '[ -e "$2" ] && printf "%s\t%s\n" $(($(cat "$2/.mb") * 1024)) "$2"',
    "df": 'echo "Filesystem 1024-blocks Used Available Capacity Mounted"; echo "/dev/root 1 1 $((STUB_FREE_MB * 1024)) 1% /"',
}

A = "src-tauri/src/a.rs:1:1: replace a -> bool with true"
B = "src-tauri/src/a.rs:2:1: replace b -> bool with false"


def bash_has_mapfile() -> bool:
    return subprocess.run(["bash", "-c", "mapfile -t x < /dev/null"], capture_output=True).returncode == 0


class MutationShRust(unittest.TestCase):
    def run_sh(self, cfg, baseline=None, free_mb=300000, target_mb=6000, pre_target_mb=None):
        d = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, d)
        bin_ = d / "bin"
        bin_.mkdir()
        for name, body in TOOLS.items():
            (bin_ / name).write_text(f"#!/bin/sh\n{body}\n")
            (bin_ / name).chmod(0o755)
        stub = bin_ / "cargo-mutants"
        stub.write_text(STUB)
        stub.chmod(0o755)
        (d / "cfg.json").write_text(json.dumps(cfg))
        (d / "pr.diff").write_text("")
        work = d / "work"
        (work / "src-tauri" / "src").mkdir(parents=True)
        if pre_target_mb is not None:
            (work / "target").mkdir()
            (work / "target" / ".mb").write_text(f"{pre_target_mb}\n")
        env = {**os.environ, "PATH": f"{bin_}:{os.environ['PATH']}", "CARGO_MUTANTS": str(stub),
               "RUNNER_TEMP": str(d), "STUB_CFG": str(d / "cfg.json"), "STUB_LOG": str(d / "calls.log"),
               "STUB_FREE_MB": str(free_mb), "STUB_TARGET_MB": str(target_mb)}
        env.pop("MUTATION_WARM_TARGET_MB", None)
        env.pop("MUTATION_BASELINE", None)
        # As on GitHub: dtolnay/rust-toolchain sets it for every later step.
        env["CARGO_TERM_COLOR"] = "always"
        if baseline:
            env["MUTATION_BASELINE"] = baseline
        p = subprocess.run(["bash", str(MUTATION_SH), "rust", str(d / "pr.diff"), "0/3"],
                           cwd=work, env=env, capture_output=True, text=True, timeout=60)
        calls = (d / "calls.log").read_text().splitlines()
        runs = [c for c in calls if "--list" not in c and "--version" not in c and not c.startswith("cargo ")]
        self.warm = [c for c in calls if c.startswith("cargo ")]
        return p, runs, work

    def test_a_shard_with_no_mutants_is_green_and_runs_nothing(self):
        p, runs, _ = self.run_sh({"all": [A, B], "shard": [], "pass1": {}, "pass2": {}})
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertIn("mutation: no mutants on shard 0/3", p.stdout)
        self.assertEqual(runs, [])

    def test_a_failing_shard_listing_stays_red(self):
        p, runs, _ = self.run_sh({"all": [A], "shard": [A], "list_rc": 1, "pass1": {}, "pass2": {}})
        self.assertNotEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual(runs, [])

    def test_a_red_baseline_stays_red_and_pass_2_never_skips_its_baseline(self):
        p, runs, _ = self.run_sh({"all": [A, B], "shard": [A, B], "red_tree": True, "pass1": {}, "pass2": {}},
                                 baseline="skip")
        self.assertEqual(p.returncode, 4, p.stdout + p.stderr)
        self.assertEqual(len(runs), 2, runs)
        self.assertIn("--test-tool nextest", runs[0])
        self.assertIn("--output mutants.full", runs[1])
        self.assertNotIn("--baseline skip", runs[1])
        self.assertIn("baseline run", p.stdout)

    def test_a_shard_pass_1_catches_ends_after_pass_1(self):
        p, runs, work = self.run_sh({"all": [A, B], "shard": [A, B], "pass1": {}, "pass2": {}}, baseline="skip")
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual(len(runs), 1, runs)
        self.assertIn("mutation: pass 1 caught every mutant with a failing test", p.stdout)
        self.assertEqual((work / "mutants.out" / "caught.txt").read_text(), f"{A}\n{B}\n")
        self.assertIn("mutation: pass 1 proved 2 of 2 caught mutants by a failing test", p.stderr)

    def test_cargo_mutants_runs_with_plain_logs_when_the_job_asks_for_colour(self):
        p, runs, work = self.run_sh({"all": [A], "shard": [A], "pass1": {}, "pass2": {}})
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        colors = (work.parent / "calls.log.color").read_text().split()
        self.assertTrue(colors, "the stub never ran")
        self.assertEqual(set(colors), {"never"})

    @unittest.skipUnless(bash_has_mapfile(), "bash has no mapfile (macOS /bin/bash 3.2); gate/lint runs it on Linux")
    def test_a_missed_mutant_goes_to_pass_2_alone_with_the_requested_skip_and_stays_red(self):
        p, runs, work = self.run_sh({"all": [A, B], "shard": [A, B], "pass1": {B: "MissedMutant"},
                                     "pass2": {B: "MissedMutant"}}, baseline="skip")
        self.assertEqual(p.returncode, 2, p.stdout + p.stderr)
        self.assertEqual(len(runs), 2, runs)
        self.assertIn("--baseline skip", runs[1])
        self.assertNotIn("--shard", runs[1])
        self.assertIn("--re", runs[1])
        self.assertEqual((work / "mutants.out" / "missed.txt").read_text(), f"{B}\n")
        self.assertEqual((work / "mutants.out" / "caught.txt").read_text(), f"{A}\n")

    @unittest.skipUnless(bash_has_mapfile(), "bash has no mapfile (macOS /bin/bash 3.2); gate/lint runs it on Linux")
    def test_a_mutant_nextest_timed_out_is_not_caught_unless_cargo_test_catches_it(self):
        # #618 review r1 m2: pass 1 counted nextest's TIMEOUT line as a proven catch, so a
        # hang that the old single cargo-mutants run reported as Timeout (red) went green.
        p, runs, work = self.run_sh({"all": [A, B], "shard": [A, B], "pass1": {B: "NextestTimeout"},
                                     "pass2": {B: "Timeout"}})
        self.assertEqual(p.returncode, 3, p.stdout + p.stderr)
        self.assertEqual(len(runs), 2, runs)
        self.assertIn("--re", runs[1])
        self.assertIn("baseline run", p.stdout)
        self.assertEqual((work / "mutants.out" / "timeout.txt").read_text(), f"{B}\n")
        self.assertEqual((work / "mutants.out" / "caught.txt").read_text(), f"{A}\n")

    @unittest.skipUnless(bash_has_mapfile(), "bash has no mapfile (macOS /bin/bash 3.2); gate/lint runs it on Linux")
    def test_a_mutant_nextest_timed_out_is_caught_when_cargo_test_catches_it(self):
        p, runs, work = self.run_sh({"all": [A, B], "shard": [A, B], "pass1": {B: "NextestTimeout"},
                                     "pass2": {}})
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual(len(runs), 2, runs)
        self.assertEqual((work / "mutants.out" / "caught.txt").read_text(), f"{A}\n{B}\n")


class MutationShWarmBuild(unittest.TestCase):
    """#618 review r1 m1: the warm build only pays when cargo-mutants can copy its target/."""
    CFG = {"all": [A], "shard": [A], "pass1": {}, "pass2": {}}
    run_sh = MutationShRust.run_sh

    def test_a_disk_that_holds_the_copies_warms_and_copies(self):
        # The numbers of the GitHub run 42969715625 (jobs 2 there; nproc 4 here gives 2 too).
        p, runs, work = self.run_sh(self.CFG, free_mb=80374 + 5939, target_mb=5939)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual(self.warm, ["cargo test --no-run --package=app --quiet"])
        self.assertIn("--copy-target=true", runs[0])
        self.assertTrue((work / "target").is_dir())

    def test_a_disk_too_small_for_the_estimate_skips_the_warm_build(self):
        p, runs, work = self.run_sh(self.CFG, free_mb=20000)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual(self.warm, [])
        self.assertIn("mutation: warm build skipped, free 20000 MB, needs 30000 MB for jobs 2", p.stdout)
        self.assertIn("--copy-target=false", runs[0])

    def test_a_warm_target_bigger_than_the_estimate_is_removed_when_it_does_not_fit(self):
        p, runs, work = self.run_sh(self.CFG, free_mb=31000, target_mb=9000)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual(len(self.warm), 1, self.warm)
        self.assertIn("--copy-target=false", runs[0])
        self.assertFalse((work / "target").exists())

    def test_a_target_that_was_there_before_is_kept(self):
        # A cache or a box checkout's own target/: never delete what mutation.sh did not build.
        p, runs, work = self.run_sh(self.CFG, free_mb=31000, target_mb=9000, pre_target_mb=100)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual(len(self.warm), 1, self.warm)
        self.assertIn("--copy-target=false", runs[0])
        self.assertTrue((work / "target").is_dir())


class GateMutationBaseline(unittest.TestCase):
    def test_github_never_asks_mutation_sh_to_skip_its_baseline(self):
        # ci.yml's check job tests the merge ref in place after npm ci, so its green does not
        # prove the baseline of the head that cargo-mutants copies (#618 review r1 M1).
        code = [l.split("#", 1)[0] for l in GATE_MUTATION.read_text().splitlines()]
        self.assertEqual([l for l in code if "MUTATION_BASELINE" in l or "baseline=skip" in l], [])


if __name__ == "__main__":
    unittest.main()
