#!/usr/bin/env python3
"""Whether a maintainer's `release now` label makes main's head due at once.

publish-crate.yml releases every PUBLISH_EVERY versions (publish_cadence.py).
A merge users should not wait that long for gets the `release now` label on
its pull request instead: the first version that includes it then goes to
crates.io and to a GitHub release as soon as version-bump.yml cuts it, the
way code-graph-rag ships a pull request carrying the label of the same name.

Only a label whose last application was by a repository admin or maintainer
counts. A checkbox in the description would not do, since any author, a
fork's included, can edit their own. Triage access is not enough either, and
neither is a bot or a workflow token that can write pull requests. A label
from anyone else is ignored with a warning and a line in the job summary.

The merges looked at are the code of the versions not yet published: main's
first-parent commits after the one that set the newest published version, up
to the one that set head's (release.py's `commit`, which is what
publish-crate.yml publishes). A merge after that one is not in head's version
yet, so its label waits for the version that includes it. A version the label
made due that then failed to publish is still due on the next push, since
the newest published version has not moved.

Only the labelled pull requests are looked up, never every commit: this runs
on every push to main, and a call per commit since the last release would
spend the workflow token's hourly budget on a busy day.

Usage (CI, in a full clone of main):
  release_now.py <newest published version>

Reads GH_TOKEN, GITHUB_REPOSITORY and GITHUB_API_URL, and appends what it
decided to GITHUB_STEP_SUMMARY when that is set. Prints `release_now=true` or
`release_now=false` for $GITHUB_OUTPUT. Exit status 2 means it refused: a
malformed version, a history it cannot place the version in, or a lookup that
failed, which must not pass for "nobody asked". A refusal prints nothing on
stdout.
"""

from __future__ import annotations

import datetime as dt
import json
import os
import re
import subprocess
import sys
import urllib.parse
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from next_version import VERSION  # noqa: E402
from release import ReleaseError, git, package_version  # noqa: E402

LABEL = "release now"
ROLES = ("admin", "maintain")
API_VERSION = "2026-03-10"
# A pull request merged after the newest published version was set was also
# updated after it. The margin covers a runner clock, which dated the release
# commit, running ahead of GitHub's.
MARGIN = dt.timedelta(days=1)
REPOSITORY = re.compile(r"[A-Za-z0-9-]+/[A-Za-z0-9._-]+")
LOGIN = re.compile(r"[A-Za-z0-9-]+")
NEXT = re.compile(r'<([^>]+)>;\s*rel="next"')


class ReleaseNowError(Exception):
    """An input or lookup the verdict must not be made without."""


def unreleased(latest: str, cwd=None) -> tuple[list[str], dt.datetime | None]:
    """The commits of the versions after `latest`, newest first, and when
    `latest` was set.

    Walks HEAD's first-parent chain. The merges before the commit that set
    head's version carry it but are not in it, and the walk stops at the
    commit that set `latest`: the newest one carrying it whose first parent
    carries another version. Merges between that one and the next version's
    commit carry `latest` too, but their code went into the next version.
    """
    if not VERSION.fullmatch(latest):
        raise ReleaseNowError(f"malformed version {latest!r}")
    chain = git("rev-list", "--first-parent", "HEAD", cwd=cwd).split()
    versions: list[str] = []

    def version(i: int) -> str:
        while len(versions) <= i:
            commit = chain[len(versions)]
            versions.append(package_version(git("show", f"{commit}:Cargo.toml", cwd=cwd)))
        return versions[i]

    head = version(0)
    if head == latest:
        return [], None
    start = 0
    while start + 1 < len(chain) and version(start + 1) == head:
        start += 1
    commits = []
    for i in range(start, len(chain)):
        root = i + 1 == len(chain)
        if version(i) == latest and (root or version(i + 1) != latest):
            # In a shallow clone the last commit is only where the history
            # was cut off, which can be a merge after the one that set it.
            if root and git("rev-parse", "--is-shallow-repository", cwd=cwd).strip() == "true":
                raise ReleaseNowError(
                    f"the clone is shallow and ends at a commit carrying {latest}; "
                    "fetch the full history to find the commit that set it"
                )
            set_at = git("show", "-s", "--format=%cI", chain[i], cwd=cwd).strip()
            return commits, dt.datetime.fromisoformat(set_at)
        commits.append(chain[i])
    raise ReleaseNowError(f"no commit on HEAD's first-parent chain carries {latest}")


def fetch(url: str, token: str) -> tuple[bytes, str]:
    """One GET: the body and the Link header."""
    request = urllib.request.Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": API_VERSION,
            "User-Agent": "croft release_now (github.com/vitali87/croft)",
        },
    )
    with urllib.request.urlopen(request, timeout=30) as response:  # noqa: S310
        return response.read(), response.headers.get("Link") or ""


class Api:
    """The repository's REST API, every call under repos/<repository>/."""

    def __init__(self, base: str, repository: str, token: str, get=fetch):
        if not REPOSITORY.fullmatch(repository):
            raise ReleaseNowError(f"not a repository: {repository!r}")
        self.root = f"{base.rstrip('/')}/repos/{repository}/"
        self.token = token
        self.get = get

    def call(self, path: str, **query):
        """The decoded body, and for a list every page of it."""
        url = self.root + path
        if query:
            url += "?" + urllib.parse.urlencode(query, quote_via=urllib.parse.quote)
        pages = []
        while url:
            try:
                body, link = self.get(url, self.token)
                data = json.loads(body)
            except (OSError, ValueError) as err:
                raise ReleaseNowError(f"GET {url} failed: {err}") from err
            if not isinstance(data, list):
                return data
            pages.extend(data)
            found = NEXT.search(link)
            url = found.group(1) if found else ""
            # The token goes with every page, so it never leaves this API.
            if url and not url.startswith(self.root):
                raise ReleaseNowError(f"refusing a next page outside {self.root}: {url}")
        return pages

    def object(self, path: str) -> dict:
        data = self.call(path)
        if not isinstance(data, dict):
            raise ReleaseNowError(f"GET {self.root}{path} did not answer an object")
        return data


def labeller(events: list) -> tuple[str, str]:
    """Who applied LABEL last, as (type, login); empty with no record of it."""
    last = ("", "")
    for event in events:
        if not isinstance(event, dict) or event.get("event") != "labeled":
            continue
        if (event.get("label") or {}).get("name") != LABEL:
            continue
        actor = event.get("actor") or {}
        last = (str(actor.get("type") or ""), str(actor.get("login") or ""))
    return last


def decide(api: Api, commits: list[str], set_at: dt.datetime | None) -> tuple[bool, list[str]]:
    """Whether a maintainer labelled a merge among `commits`, and the lines
    that say why for the job summary."""
    if not commits:
        return False, []
    since = (set_at - MARGIN).astimezone(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    shipped = set(commits)
    lines = []
    for issue in api.call("issues", labels=LABEL, state="closed", since=since, per_page=100):
        if not isinstance(issue, dict):
            continue
        number = issue.get("number")
        pull = issue.get("pull_request")
        labels = [label.get("name") for label in issue.get("labels") or [] if isinstance(label, dict)]
        if type(number) is not int or number < 1 or LABEL not in labels:
            continue
        # Closed without merging, or an issue: nothing of it ships.
        if not isinstance(pull, dict) or not pull.get("merged_at"):
            continue
        if api.object(f"pulls/{number}").get("merge_commit_sha") not in shipped:
            continue
        kind, login = labeller(api.call(f"issues/{number}/events", per_page=100))
        role = ""
        if kind == "User" and LOGIN.fullmatch(login):
            role = str(api.object(f"collaborators/{login}/permission").get("role_name") or "")
        if role in ROLES:
            return True, [f"- **Release now:** #{number}, labelled by @{login} ({role})"]
        who = f"{kind} {login}".strip() or "no recorded event"
        print(
            f"::warning::#{number} was labelled {LABEL} by '{who}', not a maintainer; the label is ignored",
            file=sys.stderr,
        )
        lines.append(f"- **Release now:** #{number} ignored, labelled by '{who}', not a maintainer")
    return False, lines


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print(__doc__, file=sys.stderr)
        return 2
    try:
        commits, set_at = unreleased(argv[0])
        api = Api(
            os.environ.get("GITHUB_API_URL") or "https://api.github.com",
            os.environ.get("GITHUB_REPOSITORY", ""),
            os.environ.get("GH_TOKEN", ""),
            get=fetch,
        )
        due, lines = decide(api, commits, set_at)
    except (ReleaseNowError, ReleaseError, subprocess.CalledProcessError) as err:
        print(f"::error::release_now: {err}", file=sys.stderr)
        return 2
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary and lines:
        with open(summary, "a") as out:
            out.write("".join(line + "\n" for line in lines))
    print(f"release_now={str(due).lower()}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
