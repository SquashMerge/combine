"""Decide whether a failed `trellis rules check` on CI is trellis still building.

    python3 -I -B tools/trellis-check/classify.py <exit-status> <stdout-file> <stderr-file>

Exits 0, printing the code, when the failure is trellis 0.2.2's typed cold-start answer:
`index_not_ready` (the first index outlasted the CLI's 90 s wait) or `deadline_exceeded` (the
architecture build outlasted its 30 s wait, or the reply its bound). Exits 1, saying why on
stderr, for anything else, so the caller fails at once.

Those two arrive as a daemon reply whose `error` is a string, which the CLI turns into its
error before printing anything: stdout is empty, the process exits 1, and stderr ends with
`Error: <code>` or `Error: <code>: <message>`, after at most the CLI's own progress lines.
A deviation or an evaluation error prints the JSON report first, so any stdout is not
retried. Nothing else is guessed: other codes, extra stderr, undecodable bytes or another
exit status are all "don't retry".
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

RETRYABLE = ("index_not_ready", "deadline_exceeded")
ERROR_LINE = re.compile(r"Error: (?P<code>[a-z_]+)(?:: .*)?")
PROGRESS_LINE = re.compile(r"trellis: indexing this checkout \(\d+s elapsed, waiting up to \d+s\)")


def classify(status: int, stdout: bytes, stderr: bytes) -> tuple[str | None, str]:
    """(the retryable code, "") or (None, the reason not to retry)."""
    if status != 1:
        return None, f"exit status {status}, not 1"
    if stdout.strip():
        return None, "trellis printed a report, so it answered"
    try:
        text = stderr.decode("utf-8")
    except UnicodeDecodeError:
        return None, "stderr is not UTF-8"
    lines = text.splitlines()
    while lines and not lines[-1].strip():
        lines.pop()
    if not lines:
        return None, "no error on stderr"
    match = ERROR_LINE.fullmatch(lines[-1])
    if match is None:
        return None, "stderr does not end with a typed error"
    code = match.group("code")
    if code not in RETRYABLE:
        return None, f"error code {code} is not retried"
    for line in lines[:-1]:
        if line.strip() and PROGRESS_LINE.fullmatch(line) is None:
            return None, "stderr has more than trellis's progress and its error"
    return code, ""


def main(argv: list[str]) -> int:
    if len(argv) != 4 or not argv[1].isdigit():
        print("usage: classify.py <exit-status> <stdout-file> <stderr-file>", file=sys.stderr)
        return 2
    code, reason = classify(int(argv[1]), Path(argv[2]).read_bytes(), Path(argv[3]).read_bytes())
    if code is None:
        print(f"verify: not retrying trellis: {reason}", file=sys.stderr)
        return 1
    print(code)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
