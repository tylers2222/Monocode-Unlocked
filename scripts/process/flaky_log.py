"""The failed test names, repeats and merge ref in one GitHub Actions job log.

Used by flaky_tests.py. Pure text in, plain data out.
"""
import collections
import re

NETWORK = re.compile(r"curl: \(\d+\)(?: The requested URL returned error: \d+)?|HTTP error 5\d\d|\b50[234] (Bad Gateway|Service|Gateway)|error: 50[234]|ECONNRESET|ETIMEDOUT"
                     r"|Could not resolve host|TLS handshake|Temporary failure resolving|npm ERR! network", re.I)
LOG_LINE = re.compile(r"^\S+Z\s?")
ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
MERGE = re.compile(r"Merge ([0-9a-f]{40}) into ([0-9a-f]{40})")
SECTION = re.compile(r"^== scenarios: (.+?) #(\d+)\s*$")
ANY_SECTION = re.compile(r"^== ")
EVAL_ERROR = re.compile(r"^\[giga-eval [\d:]+\] ERROR: (?:timed out after \d+ s: )?(.+)$")
EVAL_COUNT = re.compile(r"^\[giga-eval [\d:]+\] .+: (\d+)/(\d+) passed\s*$")
SCENARIO_FAIL = re.compile(r"^\s+FAIL (.+)$")
PW_HEAD = re.compile(r"^\s*\d+ (failed|flaky)\s*$")
PW_TEST = re.compile(r"^\s*\[(\w+)\] › (.+?):\d+:\d+ › (.+?)\s*$")
CARGO = re.compile(r"^test (\S+) \.\.\. FAILED\s*$")
VITEST = re.compile(r"^ FAIL  (.+?)(?: \[ .+ \])?\s*$")
UNITTEST = re.compile(r"^(?:FAIL|ERROR): (\S+) \((.+?)\)")
NODE = re.compile(r"^✖ (?!failing tests)(.+?) \(\d")


def clean_scenario_text(text):
    text = UUID.sub("*", text)
    if text.startswith("instance exited"):
        return "instance exited"
    match = re.match(r"(.+):\s*[^:]*, expected\b", text)
    return (match.group(1) if match else text).strip()[:120]


def parse_log(text):
    """Failed test names, repeats and the merge ref in one job's log."""
    out = {"ref": None, "tests": set(), "network": None, "repeats": {}}
    runs = collections.defaultdict(set)   # scenario -> repeat numbers seen
    failed = collections.defaultdict(lambda: collections.defaultdict(set))  # scenario -> test -> repeats
    section, number, in_summary = None, None, False
    for raw in text.splitlines():
        line = ANSI.sub("", LOG_LINE.sub("", raw))
        merge = MERGE.search(line)
        if merge and not out["ref"]:
            out["ref"] = merge.group(1, 2)
        if not out["network"]:
            hit = NETWORK.search(line)
            out["network"] = hit.group(0).replace("The requested URL returned error:", "HTTP") if hit else None
        head = SECTION.match(line)
        if head:
            section, number = head.group(1), int(head.group(2))
            runs[section].add(number)
            continue
        if ANY_SECTION.match(line):
            section = None
        if section:
            hit = EVAL_ERROR.match(line) or SCENARIO_FAIL.match(line)
            if hit:
                failed[section][clean_scenario_text(hit.group(1))].add(number)
                continue
            count = EVAL_COUNT.match(line)
            if count and int(count.group(1)) < int(count.group(2)) and not any(number in r for r in failed[section].values()):
                failed[section]["failed, no detail in the log"].add(number)
        if PW_HEAD.match(line):
            in_summary = True
            continue
        if in_summary:
            pw = PW_TEST.match(line)
            if pw:
                out["tests"].add("browser: %s › %s" % (pw.group(2).replace("tests/browser/", ""), pw.group(3)))
                continue
            in_summary = False
        for rule, make in ((CARGO, lambda m: "cargo: " + m.group(1)),
                           (VITEST, lambda m: "vitest: " + m.group(1)),
                           (UNITTEST, lambda m: "unittest: %s.%s" % (m.group(2), m.group(1))),
                           (NODE, lambda m: "node: " + m.group(1))):
            hit = rule.match(line)
            if hit:
                out["tests"].add(make(hit))
                break
    for scenario, tests in failed.items():
        bad = set().union(*tests.values()) if tests else set()
        clean = len(runs[scenario] - bad)
        for test, numbers in tests.items():
            name = "scenario %s: %s" % (scenario, test)
            out["tests"].add(name)
            if clean and numbers:
                out["repeats"][name] = (len(numbers), len(runs[scenario]))
    return out
