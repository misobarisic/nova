#!/usr/bin/env python3
"""Run Rust/Python regression gates, rejecting empty successful discovery."""
import argparse
import re
import subprocess
import sys
import unittest


def rust(command):
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                               text=True, bufsize=1)
    counts = []
    for line in process.stdout:
        print(line, end="", flush=True)
        match = re.search(r"test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;", line)
        if match:
            counts.append(tuple(map(int, match.groups())))
    status = process.wait()
    if status:
        return status
    if not counts or any(passed == 0 or failed for passed, failed, _ in counts):
        print("CI gate requires nonempty passing tests in every selected target", file=sys.stderr)
        return 1
    print(f"CI gate: {sum(count[0] for count in counts)} passed across {len(counts)} targets")
    return 0


def python_suite(directory, pattern):
    suite = unittest.defaultTestLoader.discover(directory, pattern=pattern)
    if suite.countTestCases() == 0:
        print(f"CI gate discovered zero tests in {directory}", file=sys.stderr)
        return 1
    return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--discover")
    parser.add_argument("--pattern", default="test_*.py")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.discover:
        if args.command:
            parser.error("discovery cannot be combined with a command")
        return python_suite(args.discover, args.pattern)
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("provide --discover or a Rust test command after --")
    return rust(command)


if __name__ == "__main__":
    sys.exit(main())
