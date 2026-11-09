#!/usr/bin/env python3
"""ci-verdict: the result of the required checks `ci` (ci.yml) and `e2e-linux` (e2e.yml).

Usage:
  ci_verdict.py <ci|e2e>
      Reads NEEDS (toJSON(needs)) and EVENT, and DISPATCH_SKIPS_MACOS for e2e. Prints
      `failed=<jobs>` (for GITHUB_OUTPUT) and each job's result on stderr. Exits 1 when a
      job failed.

A job passes only by success, or by a skip with a reason named in `reasons` below. Any
other skip fails, and so does a job this file does not name: GitHub counts a skipped
required check as passed, so these two checks must judge every skip themselves.

The reasons:
  - The path map (ci_changes.py) said false for the job. That output is only `false` when
    no changed file matches the job's globs or `all`, and every changed file matches a glob.
  - level=fast (ci_level.py). That run reports under another name, so it passes no required
    check. On a ready PR the level is full, so a job skipped by level fails.
  - The event: packaging runs after the merge only, and a dispatch picks its macOS build.
"""
import json
import os
import sys
from collections.abc import Mapping


def reasons(workflow: str, outputs: Mapping[str, str], event: str, dispatch_skips_macos: bool) -> dict[str, bool]:
    """Each job this check judges, and whether a skip of it has a reason."""
    fast = outputs.get("level") == "fast"

    def off(*keys: str) -> bool:
        return all(outputs.get(key) == "false" for key in keys)

    if workflow == "ci":
        return {
            "mobile": off("mobile"),
            "browser": fast or off("browser"),
            "host": off("host"),
            "check": off("check", "release-channel"),
            "appimage": event != "push" or off("appimage"),
            "rpm": event != "push" or off("rpm"),
            "rpm-clean-install": event != "push" or off("rpm"),
        }
    macos = fast or off("e2e-macos") or dispatch_skips_macos
    return {"e2e-linux-run": fast or off("e2e-linux"), "e2e-macos-1": macos, "e2e-macos-2": macos}


def failed(workflow: str, needs: Mapping[str, Mapping], event: str, dispatch_skips_macos: bool = False) -> list[str]:
    """The jobs in needs whose result does not pass. `changes` has no skip reason."""
    outputs = needs.get("changes", {}).get("outputs", {})
    skip_ok = reasons(workflow, outputs, event, dispatch_skips_macos)
    return [job for job, need in needs.items()
            if need["result"] != ("skipped" if skip_ok.get(job, False) else "success")]


def main(argv: list[str]) -> int:
    if argv not in (["ci"], ["e2e"]):
        print(__doc__, file=sys.stderr)
        return 2
    needs = json.loads(os.environ["NEEDS"])
    for job, need in needs.items():
        print(f"{job}: {need['result']}", file=sys.stderr)
    bad = failed(argv[0], needs, os.environ["EVENT"], os.environ.get("DISPATCH_SKIPS_MACOS") == "true")
    print(f"failed={', '.join(bad)}")
    if bad:
        print(f"Failed, cancelled, or skipped with no reason: {', '.join(bad)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
