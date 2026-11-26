"""Reads GitHub's check runs for one commit and decides what tools/ci_verify.sh does next.

    python3 -I -B evaluate.py checks <pages> <floor> <name> [<name>...]
    python3 -I -B evaluate.py annotations <pages>

`checks` reads <pages>, the output of
`gh api --paginate repos/OWNER/NAME/commits/SHA/check-runs?filter=all`: one JSON object per page,
one after another. It keeps only GitHub Actions check runs with one of the required names and an
id above <floor>, and takes the newest (highest id) run for each name. An older run never counts,
whatever it says. It prints

    <verdict>          pass, fail, infra, missing or pending
    <max id>           the highest id among the required names' runs, 0 if none
    <name>\t<id>\t<state>\t<url>    one line per required name, in the order given

where <state> is `missing`, a check run status that is not completed (`queued`, `in_progress`,
...), `passed`, `failed:<conclusion>` or `infra:<conclusion>`. The verdict is `fail` if any
newest run failed, otherwise `infra` if any was stopped by the platform rather than by the code
(INFRA_CONCLUSIONS), otherwise `missing` if any name has no run, otherwise `pending` if any is
still running, otherwise `pass`. So `pass` needs every required name present and successful.

`annotations` reads the paginated annotations of one failed check run and prints `infra` when
one of them is a runner failure GitHub reports as a failed job (RUNNER_FAILURES), else `fail`.

Exit status 0 on an answer; 2 on unreadable input, which the caller treats as a failed poll.
"""

from __future__ import annotations

import json
import sys

# Conclusions that say the job never ran to an answer of its own: cancelled (by a person, by
# concurrency or by the platform), the workflow failing to start, GitHub marking it stale, or a
# run waiting for approval. A fresh run can fix these; they are never a pass or a fail.
INFRA_CONCLUSIONS = {"cancelled", "startup_failure", "stale", "action_required"}

# Messages GitHub puts on a job that failed because of its runner, not its steps.
RUNNER_FAILURES = (
    "was not acquired by runner",
    "runner has received a shutdown signal",
    "lost communication with the server",
)


class BadInput(Exception):
    pass


def read_pages(path: str) -> list:
    try:
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
    except (OSError, UnicodeDecodeError) as error:
        raise BadInput(str(error)) from error
    decoder = json.JSONDecoder()
    pages, at = [], 0
    while True:
        while at < len(text) and text[at].isspace():
            at += 1
        if at == len(text):
            break
        try:
            page, at = decoder.raw_decode(text, at)
        except ValueError as error:
            raise BadInput(f"not JSON at offset {at}: {error}") from error
        pages.append(page)
    if not pages:
        raise BadInput("no pages")
    return pages


def check_runs(pages: list) -> list:
    runs = []
    for page in pages:
        if not isinstance(page, dict) or not isinstance(page.get("check_runs"), list):
            raise BadInput("a page has no check_runs list")
        runs.extend(page["check_runs"])
    return runs


def state_of(run: dict) -> str:
    status = run.get("status")
    if status != "completed":
        return str(status or "unknown")
    conclusion = run.get("conclusion")
    if conclusion == "success":
        return "passed"
    if conclusion in INFRA_CONCLUSIONS:
        return f"infra:{conclusion}"
    return f"failed:{conclusion}"


def evaluate(pages: list, floor: int, names: list[str]) -> list[str]:
    newest: dict[str, dict] = {}
    max_id = 0
    for run in check_runs(pages):
        if not isinstance(run, dict):
            raise BadInput("a check run is not an object")
        app = run.get("app") if isinstance(run.get("app"), dict) else {}
        run_id = run.get("id")
        name = run.get("name")
        if app.get("slug") != "github-actions" or name not in names:
            continue
        if not isinstance(run_id, int) or isinstance(run_id, bool):
            raise BadInput("a check run has no integer id")
        max_id = max(max_id, run_id)
        if run_id <= floor:
            continue
        if name not in newest or run_id > newest[name]["id"]:
            newest[name] = run

    lines, states = [], []
    for name in names:
        run = newest.get(name)
        if run is None:
            state, run_id, url = "missing", "-", "-"
        else:
            state = state_of(run)
            run_id = str(run["id"])
            url = run.get("details_url") or run.get("html_url") or "-"
        states.append(state)
        lines.append(f"{name}\t{run_id}\t{state}\t{url}")

    if any(s.startswith("failed:") for s in states):
        verdict = "fail"
    elif any(s.startswith("infra:") for s in states):
        verdict = "infra"
    elif "missing" in states:
        verdict = "missing"
    elif any(s != "passed" for s in states):
        verdict = "pending"
    else:
        verdict = "pass"
    return [verdict, str(max_id), *lines]


def annotations(pages: list) -> str:
    for page in pages:
        if not isinstance(page, list):
            raise BadInput("an annotations page is not a list")
        for note in page:
            message = str(note.get("message", "")) if isinstance(note, dict) else ""
            if any(text in message.lower() for text in RUNNER_FAILURES):
                return "infra"
    return "fail"


def main(argv: list[str]) -> int:
    try:
        if len(argv) >= 5 and argv[1] == "checks":
            floor = int(argv[3])
            names = argv[4:]
            if len(set(names)) != len(names) or not all(names):
                raise BadInput("required names must be distinct and non-empty")
            print("\n".join(evaluate(read_pages(argv[2]), floor, names)))
            return 0
        if len(argv) == 3 and argv[1] == "annotations":
            print(annotations(read_pages(argv[2])))
            return 0
    except (BadInput, ValueError) as error:
        print(f"ci-verify: unreadable GitHub answer: {error}", file=sys.stderr)
        return 2
    print("\n".join(__doc__.strip().splitlines()[2:4]), file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
