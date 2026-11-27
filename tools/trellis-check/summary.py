"""Print a short summary of a `trellis rules check --format json` report.

    python3 -I -B tools/trellis-check/summary.py <report.json>

./tools/verify and ci-check.sh run it after every check that printed a report, so the counts
and the files in no layer are in the log of every change. AGENTS.md asks for a review of new
`unknown` references and unassigned production files; this is what that review starts from.
It decides nothing: the caller still passes or fails on trellis's own exit status alone.

Exits 0 once it has printed the summary, and 1, saying why on stderr, when the file holds no
report to summarize (trellis failed before printing one, or printed an error instead).
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

STATUSES = (
    ("conforms", "conform"),
    ("deviates", "deviate"),
    ("excepted", "excepted"),
    ("unknown", "unknown"),
)


def counts_line(label: str, counts: dict) -> str:
    parts = [f"{counts.get(key, 0)} {word}" for key, word in STATUSES]
    return f"trellis: {label}: " + ", ".join(parts)


def summarize(report: dict) -> list[str]:
    """The summary's lines. Raises ValueError when `report` is not a rules-check report."""
    payload = report.get("payload") if isinstance(report, dict) else None
    if not isinstance(payload, dict) or not isinstance(payload.get("counts"), dict):
        raise ValueError("no payload.counts in the report")
    lines = [counts_line("production", payload["counts"])]
    if isinstance(payload.get("testCounts"), dict):
        lines.append(counts_line("test only", payload["testCounts"]))
    unassigned = payload.get("unassigned")
    unassigned = unassigned if isinstance(unassigned, list) else []
    paths = [entry.get("path") if isinstance(entry, dict) else None for entry in unassigned]
    files = sorted({path for path in paths if isinstance(path, str)})
    packages = sum(1 for path in paths if not isinstance(path, str))
    count = payload.get("unassignedFileCount", len(files))
    lines.append(f"trellis: {count} unassigned files, {packages} unassigned external packages")
    lines.extend(f"  {path}" for path in files)
    return lines


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: summary.py <report.json>", file=sys.stderr)
        return 2
    try:
        report = json.loads(Path(argv[1]).read_text(encoding="utf-8"))
        lines = summarize(report)
    except (OSError, UnicodeDecodeError, ValueError) as error:
        print(f"trellis: no summary: {error}", file=sys.stderr)
        return 1
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
