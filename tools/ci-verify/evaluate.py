"""Reads GitHub's check runs for one commit and decides what tools/ci_verify.sh does next.

    python3 -I -B evaluate.py <check-runs> <workflow-runs> <sha> <floor> <name> [<name>...]

<check-runs> is the output of
`gh api --paginate repos/OWNER/NAME/commits/SHA/check-runs?filter=all`, and <workflow-runs> of
`gh api --paginate repos/OWNER/NAME/actions/runs?head_sha=SHA&event=push`: one JSON object per
page, one after another.

Only a check run from a workflow run that a push of <sha> started counts: its check suite must
be the `check_suite_id` of a workflow run with event `push` and head_sha <sha>. Such a run
checked out <sha> itself. A pull_request run is attached to the pull request's head sha too, but
it checked out a merge commit with the base branch, a different tree, so it never counts.

Of those, it keeps GitHub Actions check runs with a required name and an id above <floor>, and
takes the newest (highest id) for each name. An older run never counts, whatever it says. It
prints

    <verdict>          pass, fail, infra, missing or pending
    <max id>           the highest id among the counted runs of the required names, 0 if none
    <name>\t<id>\t<state>\t<url>    one line per required name, in the order given

where <state> is `missing`, a check run status that is not completed (`queued`, `in_progress`,
...), `passed`, `failed:<conclusion>` or `infra:<conclusion>`. The verdict is `fail` if any
newest run failed, otherwise `infra` if any was stopped by the platform rather than by the code,
otherwise `missing` if any name has no run, otherwise `pending` if any is still running,
otherwise `pass`. So `pass` needs every required name present and successful.

A platform stop is read from the check run's conclusion alone (INFRA_CONCLUSIONS), never from
annotation or log text, which the job's own steps can write. GitHub concludes a job that no
runner picked up ("The job was not acquired by Runner...") as `cancelled`.

Exit status 0 on an answer; 2 on unreadable input, which the caller treats as a failed poll.
"""

from __future__ import annotations

import json
import sys

# Conclusions that say the job never ran to an answer of its own: cancelled (by a person, by
# concurrency, or by the platform when no runner took the job), the workflow failing to start,
# GitHub marking it stale, or a run waiting for approval. A fresh run can fix these; they are
# never a pass or a fail.
INFRA_CONCLUSIONS = {"cancelled", "startup_failure", "stale", "action_required"}


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


def items(pages: list, key: str) -> list:
    found = []
    for page in pages:
        if not isinstance(page, dict) or not isinstance(page.get(key), list):
            raise BadInput(f"a page has no {key} list")
        for item in page[key]:
            if not isinstance(item, dict):
                raise BadInput(f"an entry of {key} is not an object")
            found.append(item)
    return found


def push_suites(workflow_pages: list, sha: str) -> set:
    """Check suite ids of the workflow runs a push of `sha` started."""
    suites = set()
    for run in items(workflow_pages, "workflow_runs"):
        suite = run.get("check_suite_id")
        if run.get("event") == "push" and run.get("head_sha") == sha and isinstance(suite, int):
            suites.add(suite)
    return suites


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


def evaluate(check_pages: list, workflow_pages: list, sha: str, floor: int, names: list) -> list:
    suites = push_suites(workflow_pages, sha)
    newest: dict = {}
    max_id = 0
    for run in items(check_pages, "check_runs"):
        app = run.get("app") if isinstance(run.get("app"), dict) else {}
        suite = run.get("check_suite") if isinstance(run.get("check_suite"), dict) else {}
        name = run.get("name")
        if app.get("slug") != "github-actions" or name not in names or suite.get("id") not in suites:
            continue
        run_id = run.get("id")
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


def main(argv: list) -> int:
    if len(argv) < 6:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    try:
        _, check_path, workflow_path, sha, floor, *names = argv
        if len(set(names)) != len(names) or not all(names):
            raise BadInput("required names must be distinct and non-empty")
        lines = evaluate(read_pages(check_path), read_pages(workflow_path), sha, int(floor), names)
    except (BadInput, ValueError) as error:
        print(f"ci-verify: unreadable GitHub answer: {error}", file=sys.stderr)
        return 2
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
