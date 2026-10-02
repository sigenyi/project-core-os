#!/usr/bin/env python3
"""Compare test-suite results against a package's documented expected failures.

    compare-results.py --expected FILE --summary OUT [--min-pass N] RESULT.sum...

RESULT files are DejaGnu `.sum` files (binutils, GCC) or glibc's `tests.sum`; both
write one `STATUS: test` line per result. FAIL, XPASS, ERROR and UNRESOLVED are
bad results. Each bad result must appear, verbatim, in the expected file, where
`#` lines above an entry say why it is expected (environment, known upstream
bug). The check fails if a bad result is not expected, or if no test passed at
all (the suite did not really run). Expected entries that no longer occur are
reported, not fatal: timing-dependent tests come and go. --min-pass guards
against a suite that ran only part of its tests (a test program that failed to
build can drop whole directories without a FAIL line).

    compare-results.py --self-test
"""

import argparse
import collections
import re
import sys

RESULT = re.compile(r"^(PASS|FAIL|XFAIL|XPASS|KFAIL|KPASS|UNRESOLVED|UNSUPPORTED|UNTESTED|ERROR): (.*\S)\s*$")
BAD = {"FAIL", "XPASS", "ERROR", "UNRESOLVED"}


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
    print("self-test ok")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--expected")
    ap.add_argument("--summary")
    ap.add_argument("--min-pass", type=int, default=1)
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("results", nargs="*")
    a = ap.parse_args()
    if a.self_test:
        self_test()
        return
    if not a.expected or not a.summary or not a.results:
        ap.error("--expected, --summary and at least one result file are required")
    lines = []
    for path in a.results:
        with open(path, errors="replace") as f:
            lines += f.readlines()
    counts, bad = parse_results(lines)
    with open(a.expected) as f:
        expected = parse_expected(f)
    ok, report = compare(counts, bad, expected, a.min_pass)
    text = "\n".join(report) + "\n"
    with open(a.summary, "w") as f:
        f.write(text)
    sys.stdout.write(text)
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
