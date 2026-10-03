#!/usr/bin/env python3
"""Compare test-suite results against a package's documented expected failures.

    compare-results.py --expected FILE --summary OUT [--min-pass N]
                       [--require SUITE=MIN ...] RESULT.sum...

RESULT files are DejaGnu `.sum` files (binutils, GCC) or glibc's `tests.sum`; both
write one `STATUS: test` line per result. FAIL, XPASS, ERROR and UNRESOLVED are
bad results. Each bad result must appear, verbatim, in the expected file, where
`#` lines above an entry say why it is expected (environment, known upstream
bug). The check fails if a bad result is not expected, or if no test passed at
all (the suite did not really run). Expected entries that no longer occur are
reported, not fatal: timing-dependent tests come and go. --min-pass guards
against a run that did only part of its tests (a test program that failed to
build can drop whole directories without a FAIL line). It counts passes over all
the files, so for a recipe with several suites it cannot see one small suite
running nothing: --require SUITE=MIN checks each DejaGnu suite on its own. The
suite's file, SUITE.sum, must be among the results exactly once. It must hold one
summary, `=== SUITE Summary ===`, which DejaGnu writes when the run completes (a
run that died part-way has none), after all of its test results. After the
summary, a test result line or a line a running test run writes (`Running `,
`Test run by `, `WARNING: `, `ERROR: `) is rejected; other lines, such as the
summary's counts and the tool's version line, are allowed. Every count the
summary gives must equal the result lines of that status in the file, and at
least MIN tests must have passed. A result file may be given only once.

    compare-results.py --self-test
"""

import argparse
import collections
import os
import re
import sys

RESULT = re.compile(r"^(PASS|FAIL|XFAIL|XPASS|KFAIL|KPASS|UNRESOLVED|UNSUPPORTED|UNTESTED|ERROR): (.*\S)\s*$")
BAD = {"FAIL", "XPASS", "ERROR", "UNRESOLVED"}
SUMMARY = re.compile(r"^\s*=== .* Summary\b.*===\s*$")
FOOTER_COUNT = re.compile(r"^# of (.+?)\s+(\d+)\s*$")
# The counts DejaGnu prints in a summary, and the status each one counts.
FOOTER_STATUS = {
    "expected passes": "PASS",
    "unexpected failures": "FAIL",
    "unexpected successes": "XPASS",
    "expected failures": "XFAIL",
    "known failures": "KFAIL",
    "unknown successes": "KPASS",
    "unresolved testcases": "UNRESOLVED",
    "untested testcases": "UNTESTED",
    "unsupported tests": "UNSUPPORTED",
}
# Lines a run writes while it is still running tests.
RUNNING = re.compile(r"^(Running |Test run by |WARNING: |ERROR: )")


def parse_results(lines):
    counts = collections.Counter()
    bad = set()
    for line in lines:
        m = RESULT.match(line)
        if not m:
            continue
        status, test = m.groups()
        counts[status] += 1
        if status in BAD:
            bad.add(f"{status}: {test}")
    return counts, bad


def parse_expected(lines):
    expected = set()
    for line in lines:
        line = line.rstrip("\n")
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        m = RESULT.match(line)
        if not m or m.group(1) not in BAD:
            raise ValueError(f"not a bad-result line: {line!r}")
        expected.add(f"{m.group(1)}: {m.group(2)}")
    return expected


def parse_require(specs):
    """SUITE=MIN strings to {suite: min}."""
    required = {}
    for spec in specs:
        name, sep, minimum = spec.partition("=")
        if not sep or not name or not minimum.isdigit() or int(minimum) < 1 or name in required:
            raise ValueError(f"--require wants SUITE=MIN (MIN at least 1, each suite once), not {spec!r}")
        required[name] = int(minimum)
    return required


def read_results(paths):
    """{path: lines} for each result file; the same file twice is an error."""
    files, seen = {}, set()
    for path in paths:
        real = os.path.realpath(path)
        if real in seen:
            raise ValueError(f"{path} is given twice: its results would be counted twice")
        seen.add(real)
        with open(path, errors="replace") as f:
            files[path] = f.read().splitlines()
    return files


def check_suite(name, minimum, files):
    """Problems with required suite `name`; `files` maps each result file's path to its lines."""
    paths = [p for p in files if os.path.basename(p) == f"{name}.sum"]
    if len(paths) != 1:
        return [f"Suite {name}: expected one {name}.sum among the results, found {len(paths)}: the suite did not run."]
    path, lines = paths[0], files[paths[0]]
    summaries = [i for i, line in enumerate(lines) if SUMMARY.match(line)]
    own = re.compile(rf"^\s*=== {re.escape(name)} Summary ===\s*$")
    if len(summaries) != 1 or not own.match(lines[summaries[0]]):
        found = ", ".join(lines[i].strip() for i in summaries) or "none"
        return [f"Suite {name}: {path} must end with one '=== {name} Summary ===' (found: {found}): "
                f"the run did not finish, or the file is not this suite's alone."]
    body, footer = lines[: summaries[0]], lines[summaries[0] + 1 :]
    counts = collections.Counter(m.group(1) for m in map(RESULT.match, body) if m)
    problems = []
    reported = {}
    for line in footer:
        if RESULT.match(line) or RUNNING.match(line):
            problems.append(f"Suite {name}: test output after its summary ({line.strip()!r}): the run did not finish.")
            break
        m = FOOTER_COUNT.match(line)
        if m and m.group(1) in FOOTER_STATUS:
            if m.group(1) in reported:
                problems.append(f"Suite {name}: its summary gives '# of {m.group(1)}' twice.")
            reported[m.group(1)] = int(m.group(2))
    for label, status in FOOTER_STATUS.items():
        if reported.get(label, 0) != counts[status]:
            problems.append(f"Suite {name}: its summary reports {reported.get(label, 0)} {label}, "
                            f"the file has {counts[status]} {status} results.")
    if counts["PASS"] < minimum:
        problems.append(f"Suite {name}: only {counts['PASS']} tests passed, fewer than the {minimum} required.")
    return problems


def compare(counts, bad, expected, min_pass=1):
    """Returns (ok, report lines)."""
    unexpected = sorted(bad - expected)
    stale = sorted(expected - bad)
    report = ["Results: " + ", ".join(f"{k} {counts[k]}" for k in sorted(counts))]
    if counts["PASS"] < min_pass:
        report.append(f"Only {counts['PASS']} tests passed, fewer than the {min_pass} expected: the suite did not fully run.")
    report.append(f"Expected failures: {len(bad & expected)} of {len(expected)} documented occurred.")
    if unexpected:
        report.append(f"UNEXPECTED ({len(unexpected)}):")
        report += [f"  {u}" for u in unexpected]
    if stale:
        report.append(f"Documented failures that did not occur ({len(stale)}):")
        report += [f"  {s}" for s in stale]
    return counts["PASS"] >= max(min_pass, 1) and not unexpected, report


def self_test():
    counts, bad = parse_results([
        "PASS: a",
        "FAIL: b (test for excess errors)",
        "XFAIL: c",
        "UNSUPPORTED: d",
        "XPASS: e",
        "Running x.exp ...",
    ])
    assert counts["PASS"] == 1 and counts["FAIL"] == 1 and counts["XPASS"] == 1
    assert bad == {"FAIL: b (test for excess errors)", "XPASS: e"}
    expected = parse_expected(["# b needs a terminal", "FAIL: b (test for excess errors)", "", "FAIL: gone"])
    ok, report = compare(counts, bad, expected)
    assert not ok and "  XPASS: e" in report and "  FAIL: gone" in report
    ok, _ = compare(counts, bad, expected | {"XPASS: e"})
    assert ok
    ok, _ = compare(collections.Counter(), set(), set())
    assert not ok, "a suite with no passes must fail"
    ok, report = compare(collections.Counter(PASS=5), set(), set(), min_pass=6)
    assert not ok and "fewer than the 6 expected" in report[1]
    try:
        parse_expected(["PASS: a"])
        raise AssertionError("PASS lines are not expected failures")
    except ValueError:
        pass
    assert parse_require(["gcc=10", "libitm=40"]) == {"gcc": 10, "libitm": 40}
    for spec in (["gcc"], ["gcc=0"], ["=5"], ["gcc=x"], ["gcc=1", "gcc=2"]):
        try:
            parse_require(spec)
            raise AssertionError(f"{spec} accepted")
        except ValueError:
            pass
    head = ["Test run by tester on Sat Oct  3 03:11:25 2026", "", "Running target unix", "Running x.exp ..."]
    footer = ["", "\t\t=== libitm Summary ===", "", "# of expected passes\t\t2", "# of expected failures\t\t1",
              "/build/gcc/xgcc  version 15.2.0 (GCC) ", ""]
    done = head + ["PASS: t1", "PASS: t2", "XFAIL: t3"] + footer
    assert check_suite("libitm", 2, {"b/libitm.sum": done, "b/gcc.sum": []}) == []
    # Each way a small suite can pass the aggregate count while not having run:
    missing = check_suite("libitm", 1, {"b/gcc.sum": done})
    assert missing and "found 0" in missing[0]
    twice = check_suite("libitm", 1, {"a/libitm.sum": done, "b/libitm.sum": done})
    assert twice and "found 2" in twice[0]
    empty = head + ["", "\t\t=== libitm Summary ===", ""]
    assert any("fewer than the 40" in p for p in check_suite("libitm", 40, {"b/libitm.sum": empty}))
    died = head + ["PASS: t1", "Running y.exp ..."]
    assert "found: none" in check_suite("libitm", 1, {"b/libitm.sum": died})[0]
    # Another suite's summary does not count, nor do two summaries.
    other = [line.replace("libitm Summary", "libgomp Summary") for line in done]
    assert "found: === libgomp Summary ===" in check_suite("libitm", 1, {"b/libitm.sum": other})[0]
    assert "found: === libitm Summary ===, === libitm Summary ===" in check_suite("libitm", 1, {"b/libitm.sum": done + done})[0]
    # Results or a new run after the summary: the file kept growing after it.
    for extra in ("PASS: t9", "Running z.exp ...", "FAIL: t9", "ERROR: tcl error sourcing z.exp"):
        late = check_suite("libitm", 1, {"b/libitm.sum": done + [extra]})
        assert late and "after its summary" in late[0], extra
    # Every count in the summary must match the file's result lines.
    cut = check_suite("libitm", 1, {"b/libitm.sum": head + ["PASS: t1", "XFAIL: t3"] + footer})
    assert cut == ["Suite libitm: its summary reports 2 expected passes, the file has 1 PASS results."], cut
    hidden = check_suite("libitm", 1, {"b/libitm.sum": head + ["PASS: t1", "PASS: t2", "XFAIL: t3", "FAIL: t4"] + footer})
    assert "Suite libitm: its summary reports 0 unexpected failures, the file has 1 FAIL results." in hidden
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        sum_path = os.path.join(d, "libitm.sum")
        with open(sum_path, "w") as f:
            f.write("\n".join(done) + "\n")
        assert list(read_results([sum_path])) == [sum_path]
        for twice_given in ([sum_path, sum_path], [sum_path, os.path.join(d, ".", "libitm.sum")]):
            try:
                read_results(twice_given)
                raise AssertionError(f"{twice_given} accepted")
            except ValueError as e:
                assert "given twice" in str(e)
    under = check_suite("libitm", 3, {"b/libitm.sum": done})
    assert under == ["Suite libitm: only 2 tests passed, fewer than the 3 required."]
    print("self-test ok")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--expected")
    ap.add_argument("--summary")
    ap.add_argument("--min-pass", type=int, default=1)
    ap.add_argument("--require", action="append", default=[], metavar="SUITE=MIN")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("results", nargs="*")
    a = ap.parse_args()
    if a.self_test:
        self_test()
        return
    if not a.expected or not a.summary or not a.results:
        ap.error("--expected, --summary and at least one result file are required")
    try:
        required = parse_require(a.require)
    except ValueError as e:
        ap.error(str(e))
    try:
        files = read_results(a.results)
    except ValueError as e:
        ap.error(str(e))
    counts, bad = parse_results(line for lines in files.values() for line in lines)
    with open(a.expected) as f:
        expected = parse_expected(f)
    ok, report = compare(counts, bad, expected, a.min_pass)
    for name, minimum in required.items():
        problems = check_suite(name, minimum, files)
        ok &= not problems
        report += problems
    text = "\n".join(report) + "\n"
    with open(a.summary, "w") as f:
        f.write(text)
    sys.stdout.write(text)
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
