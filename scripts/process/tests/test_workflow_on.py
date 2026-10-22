import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from workflow_on import on_events


class OnEventsTest(unittest.TestCase):
    def test_every_form_of_on(self):
        forms = {
            "on: pull_request\n": {"pull_request"},
            "on: [push, pull_request]\n": {"push", "pull_request"},
            "on: {pull_request: {branches: [x]}, push: {}}\n": None,
            "on: {pull_request: null, push: null}\n": {"pull_request", "push"},
            "on:\n  - push\n  - pull_request_target\n": {"push", "pull_request_target"},
            "on:\n  pull_request:\n    branches: [tyler/main]\n  push:\n\njobs: {}\n": {"pull_request", "push"},
            '"on":\n  pull_request_target:  # comment\n': {"pull_request_target"},
            "name: x\njobs: {}\n": None,
            "on:\n  pull_request:\n  - push\n": None,
        }
        for text, events in forms.items():
            with self.subTest(text=text):
                self.assertEqual(on_events(text), events)


if __name__ == "__main__":
    unittest.main()
