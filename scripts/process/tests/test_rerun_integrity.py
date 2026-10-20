import re
import unittest
from pathlib import Path

WORKFLOWS = Path(__file__).resolve().parents[3] / ".github" / "workflows"


def triggers(text: str) -> set[str]:
    on = re.search(r"(?ms)^on:\n(.*?)(?=^\S)", text + "\nend:")
    return set(re.findall(r"(?m)^  ([a-z_]+):", on.group(1))) if on else set()


class RerunListTest(unittest.TestCase):
    def test_rerun_follows_every_pull_request_workflow(self):
        """merge_rule.py waits for every check on the head, so gate-integrity-rerun.yml must
        re-run gate/integrity when any pull_request workflow finishes."""
        on_pr = set()
        for path in WORKFLOWS.glob("*.yml"):
            text = path.read_text()
            if "pull_request" in triggers(text):
                on_pr.add(re.search(r"(?m)^name: (.+)$", text).group(1).strip())
        rerun = (WORKFLOWS / "gate-integrity-rerun.yml").read_text()
        listed = re.search(r"(?m)^    workflows: \[(.*)\]$", rerun).group(1)
        self.assertEqual({name.strip() for name in listed.split(",")}, on_pr)


if __name__ == "__main__":
    unittest.main()
