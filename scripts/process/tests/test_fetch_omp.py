"""scripts/fetch-omp.sh and the omp cache steps in ci.yml and e2e.yml.

The failure this covers: tyler/main run 42580086493, job 129036771058 (browser 2/6),
2026-11-09. curl got "The requested URL returned error: 504" on all 4 tries in 90 s
and the job failed in setup. The script is driven with a stub gh that answers with a
scripted list of outcomes; no network is used.
"""
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
FETCH = ROOT / "scripts" / "fetch-omp.sh"
WORKFLOWS = {
    "ci.yml": "omp-linux-x64",
    "e2e.yml": "omp-darwin-arm64",
}

# Each call pops the first line of $STUB_PLAN: "ok" writes the -O file; anything else
# is printed to stderr as gh would, and the stub exits 1.
STUB = r'''#!/usr/bin/env bash
echo "$*" >>"$STUB_LOG"
plan=$(head -n 1 "$STUB_PLAN")
tail -n +2 "$STUB_PLAN" >"$STUB_PLAN.next" && mv "$STUB_PLAN.next" "$STUB_PLAN"
if [ "$plan" = ok ]; then
  while [ "$#" -gt 0 ]; do
    if [ "$1" = -O ]; then printf 'omp-binary' >"$2"; fi
    shift
  done
  exit 0
fi
echo "$plan" >&2
exit 1
'''

GATEWAY_TIMEOUT = "HTTP 504: Gateway Timeout (https://api.github.com/repos/can1357/oh-my-pi/releases/assets/1)"


class FetchOmpTest(unittest.TestCase):
    def run_fetch(self, plan, tries=6):
        tmp = Path(tempfile.mkdtemp())
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        gh = bin_dir / "gh"
        gh.write_text(STUB)
        gh.chmod(0o755)
        (tmp / "plan").write_text("".join(p + "\n" for p in plan))
        env = dict(
            os.environ,
            PATH=f"{bin_dir}:{os.environ['PATH']}",
            STUB_PLAN=str(tmp / "plan"),
            STUB_LOG=str(tmp / "log"),
            OMP_TRIES=str(tries),
            OMP_RETRY_DELAY="0",
        )
        dest = tmp / "omp"
        proc = subprocess.run(
            ["bash", str(FETCH), "v18.2.11", "omp-linux-x64", str(dest)],
            env=env, capture_output=True, text=True, timeout=60,
        )
        log = (tmp / "log").read_text().splitlines() if (tmp / "log").exists() else []
        return proc, log, dest

    def test_a_504_is_retried_until_the_download_lands(self):
        proc, log, dest = self.run_fetch([GATEWAY_TIMEOUT] * 4 + ["ok"])
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(len(log), 5)
        self.assertIn("downloaded on try 5", proc.stdout)
        self.assertEqual(dest.read_text(), "omp-binary")
        self.assertEqual(log[0], "release download v18.2.11 -R can1357/oh-my-pi -p omp-linux-x64 -O " + str(dest) + " --clobber")

    def test_504_on_every_try_fails_after_the_last_try(self):
        proc, log, _ = self.run_fetch([GATEWAY_TIMEOUT] * 6, tries=6)
        self.assertEqual(proc.returncode, 1)
        self.assertEqual(len(log), 6)
        self.assertIn("still failing after 6 tries", proc.stderr)

    def test_network_error_502_503_and_429_are_retried(self):
        plan = [
            "Get \"https://api.github.com/...\": dial tcp: i/o timeout",
            "HTTP 502: Bad Gateway",
            "HTTP 503: Service Unavailable",
            "HTTP 429: Too Many Requests",
            "ok",
        ]
        proc, log, _ = self.run_fetch(plan)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(len(log), 5)

    def test_a_client_error_is_not_retried(self):
        for error in ["HTTP 404: Not Found", "release not found", "no assets match the file pattern"]:
            with self.subTest(error=error):
                proc, log, _ = self.run_fetch([error, "ok"])
                self.assertEqual(proc.returncode, 1)
                self.assertEqual(len(log), 1)
                self.assertIn("not retried", proc.stderr)


def steps_around_omp(text):
    """The omp step block: from the omp-cache restore to `omp --version`."""
    start = text.index("      - id: omp-cache\n")
    end = text.index("      - run: omp --version\n", start)
    return text[start:end]


class OmpCacheWiringTest(unittest.TestCase):
    def test_each_workflow_restores_downloads_checks_then_saves(self):
        for name, asset in WORKFLOWS.items():
            with self.subTest(workflow=name):
                text = (ROOT / ".github" / "workflows" / name).read_text()
                self.assertNotIn("releases/download", text, "anonymous curl download is back")
                block = steps_around_omp(text)
                order = [
                    block.index("uses: actions/cache/restore@"),
                    block.index(f"run: scripts/fetch-omp.sh v18.2.11 {asset} "),
                    block.index("-c -"),
                    block.index("uses: actions/cache/save@"),
                ]
                self.assertEqual(order, sorted(order), "restore, download, sha256 check, save")
                keys = re.findall(r"key: (\S+)", block)
                self.assertEqual(len(keys), 2)
                self.assertEqual(keys[0], keys[1])
                self.assertIn("v18.2.11", keys[0])
                download = block[block.index("- name: Download omp"):block.index("- name: Install omp")]
                self.assertIn("if: steps.omp-cache.outputs.cache-hit != 'true'", download)
                self.assertIn("GH_TOKEN: ${{ github.token }}", download)
                save = block[block.index("- name: Save omp"):]
                self.assertIn("steps.omp-cache.outputs.cache-hit != 'true'", save)

    def test_a_cache_hit_still_runs_the_sha256_check(self):
        # The Install step must run on a hit too, so a cached binary is never used unchecked.
        for name in WORKFLOWS:
            with self.subTest(workflow=name):
                block = steps_around_omp((ROOT / ".github" / "workflows" / name).read_text())
                start = block.index("      - name: Install omp")
                end = block.index("      - ", start + 1)
                install = block[start:end]
                self.assertNotRegex(install, r"(?m)^        if:", "Install omp must have no if:")
                self.assertRegex(install, r'echo "[0-9a-f]{64}  \$RUNNER_TEMP/omp" \| (sha256sum|shasum -a 256) -c -')

    def test_the_key_names_the_checked_sha256(self):
        for name in WORKFLOWS:
            with self.subTest(workflow=name):
                block = steps_around_omp((ROOT / ".github" / "workflows" / name).read_text())
                key = re.search(r"key: (\S+)", block).group(1)
                sha = re.search(r'echo "([0-9a-f]{64})  \$RUNNER_TEMP/omp"', block).group(1)
                self.assertTrue(key.endswith(sha[:8]), (key, sha))


if __name__ == "__main__":
    unittest.main()
