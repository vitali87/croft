"""Tests for scripts/release_now.py.

A maintainer's `release now` label on a merged pull request makes the first
version that includes it due at once, instead of at the 50-version cadence.
The commits come from a real throwaway git repo shaped like croft's main, and
the GitHub API from a fake that answers only the calls the script makes, so
a changed call fails here rather than passing.

The workflow step that runs the script is checked by
test_publish_crate_workflow.py.
"""

from __future__ import annotations

import datetime as dt
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

import release_now as rn  # noqa: E402

CLI = ROOT / "scripts" / "release_now.py"
TOML = '[package]\nname = "croft-software"\nversion = "{}"\n\n[dependencies.toml]\nversion = "0.2.11"\n'
API = "https://api.example/repos/vitali87/croft/"
SINCE = "since=2026-10-19T09%3A00%3A00Z"


def git(cwd, *args, env=None):
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True, env=env
    ).stdout.strip()


class Main:
    """A throwaway croft-shaped main: release commits that set each version,
    and pull requests merged with --no-ff between them, every commit dated
    2026-10-20T10:00Z so `since` is known."""

    def __init__(self, path: Path):
        self.path = path
        self.env = {
            **os.environ,
            "GIT_AUTHOR_DATE": "2026-10-20T10:00:00+01:00",
            "GIT_COMMITTER_DATE": "2026-10-20T10:00:00+01:00",
        }
        git(path, "init", "-q", "-b", "main")
        git(path, "config", "user.email", "t@example.com")
        git(path, "config", "user.name", "t")
        self.release("0.2.11")

    def commit(self, message: str) -> str:
        git(self.path, "add", "-A")
        git(self.path, "commit", "-q", "--allow-empty", "-m", message, env=self.env)
        return git(self.path, "rev-parse", "HEAD")

    def release(self, version: str) -> str:
        (self.path / "Cargo.toml").write_text(TOML.format(version))
        return self.commit(f"chore: release {version}")

    def merge(self, name: str) -> str:
        """A pull request with one commit, merged into main with --no-ff."""
        git(self.path, "checkout", "-q", "-b", name)
        (self.path / f"{name}.rs").write_text(name)
        self.commit(name)
        git(self.path, "checkout", "-q", "main")
        git(self.path, "merge", "-q", "--no-ff", "-m", f"Merge {name}", name, env=self.env)
        return git(self.path, "rev-parse", "HEAD")


@unittest.skipUnless(shutil.which("git"), "needs git")
class Unreleased(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)
        self.main = Main(self.dir)

    def test_the_versions_after_the_published_one_up_to_heads(self):
        # 0.2.11 is published. A merged after it is in 0.2.12, B in 0.2.13,
        # and C, merged after 0.2.13 was cut, is in no version yet.
        a = self.main.merge("a")
        r12 = self.main.release("0.2.12")
        b = self.main.merge("b")
        r13 = self.main.release("0.2.13")
        self.main.merge("c")
        commits, set_at = rn.unreleased("0.2.11", cwd=self.dir)
        self.assertEqual(commits, [r13, b, r12, a])
        self.assertEqual(set_at, dt.datetime.fromisoformat("2026-10-20T10:00:00+01:00"))

    def test_a_merge_carrying_the_published_version_is_in_the_next(self):
        # 0.2.12 is published; A landed after its release commit, so it
        # carries 0.2.12 but its code is 0.2.13's.
        self.main.release("0.2.12")
        a = self.main.merge("a")
        r13 = self.main.release("0.2.13")
        self.assertEqual(rn.unreleased("0.2.12", cwd=self.dir)[0], [r13, a])

    def test_a_branch_s_own_commits_are_not_walked(self):
        self.main.merge("a")
        branch_commit = git(self.dir, "rev-parse", "a")
        self.main.release("0.2.12")
        self.assertNotIn(branch_commit, rn.unreleased("0.2.11", cwd=self.dir)[0])

    def test_nothing_is_unreleased_when_head_is_the_published_version(self):
        self.main.merge("a")
        self.assertEqual(rn.unreleased("0.2.11", cwd=self.dir), ([], None))

    def test_a_version_the_chain_never_carried_is_refused(self):
        self.main.release("0.2.12")
        with self.assertRaisesRegex(rn.ReleaseNowError, "carries 0.2.10"):
            rn.unreleased("0.2.10", cwd=self.dir)

    def test_a_malformed_version_is_refused(self):
        with self.assertRaisesRegex(rn.ReleaseNowError, "malformed"):
            rn.unreleased("0.2.11;x", cwd=self.dir)

    def test_a_shallow_clone_cut_inside_the_published_version_is_refused(self):
        # Its last commit, A, carries 0.2.11 but is the merge after the one
        # that set it: stopping there would leave A out of 0.2.12.
        self.main.merge("a")
        self.main.release("0.2.12")
        clone = self.dir / "clone"
        git(self.dir, "clone", "-q", "--depth", "2", f"file://{self.dir}", str(clone))
        with self.assertRaisesRegex(rn.ReleaseNowError, "shallow"):
            rn.unreleased("0.2.11", cwd=clone)


SHA_A = "a" * 40
SHA_OUT = "e" * 40


def issue(number, merged=True, labels=(rn.LABEL,), pull=True):
    item = {"number": number, "labels": [{"name": n} for n in labels]}
    if pull:
        item["pull_request"] = {"merged_at": "2026-10-21T00:00:00Z" if merged else None}
    return item


def labelled(*actors):
    """Issue events, oldest first: each actor applied the label once."""
    events = [{"event": "merged", "actor": {"type": "User", "login": "vitali87"}}]
    for kind, login in actors:
        events.append({"event": "labeled", "label": {"name": rn.LABEL}, "actor": {"type": kind, "login": login}})
    return events


class FakeApi:
    """Serves each URL's JSON (or a list of pages, chained by Link), records
    the calls, and fails a URL listed in `fail`."""

    def __init__(self, routes, fail=()):
        self.routes = routes
        self.fail = set(fail)
        self.calls = []

    def __call__(self, url, token):
        self.calls.append(url)
        if token != "t0ken":
            raise OSError("no token")
        if url in self.fail or url not in self.routes:
            raise OSError(f"HTTP 404 for {url}")
        answer = self.routes[url]
        if isinstance(answer, tuple):
            body, link = answer
            return json.dumps(body).encode(), link
        return json.dumps(answer).encode(), ""


def listing(*issues):
    return {f"{API}issues?labels=release%20now&state=closed&{SINCE}&per_page=100": list(issues)}


def routes_for(number, merge_sha, events, roles=None):
    routes = {
        f"{API}pulls/{number}": {"number": number, "merge_commit_sha": merge_sha},
        f"{API}issues/{number}/events?per_page=100": events,
    }
    for login, role in (roles or {}).items():
        routes[f"{API}collaborators/{login}/permission"] = {"role_name": role}
    return routes


SET_AT = dt.datetime.fromisoformat("2026-10-20T10:00:00+01:00")


class Decide(unittest.TestCase):
    def decide(self, routes, commits=(SHA_A,), fail=()):
        fake = FakeApi(routes, fail)
        api = rn.Api("https://api.example", "vitali87/croft", "t0ken", get=fake)
        with redirect_stderr(io.StringIO()) as err:
            due, lines = rn.decide(api, list(commits), SET_AT)
        return due, lines, fake.calls, err.getvalue()

    def test_a_maintainer_s_label_on_a_merge_in_the_version_releases(self):
        for role in ("admin", "maintain"):
            with self.subTest(role=role):
                due, lines, _, _ = self.decide(
                    {**listing(issue(12)), **routes_for(12, SHA_A, labelled(("User", "vitali87")), {"vitali87": role})}
                )
                self.assertTrue(due)
                self.assertEqual(lines, [f"- **Release now:** #12, labelled by @vitali87 ({role})"])

    def test_anyone_else_s_label_is_ignored_and_reported(self):
        for role in ("write", "triage", "read", "none", ""):
            with self.subTest(role=role):
                due, lines, _, err = self.decide(
                    {**listing(issue(12)), **routes_for(12, SHA_A, labelled(("User", "someone")), {"someone": role})}
                )
                self.assertFalse(due)
                self.assertEqual(lines, ["- **Release now:** #12 ignored, labelled by 'User someone', not a maintainer"])
                self.assertIn("::warning::#12 was labelled release now by 'User someone'", err)

    def test_a_bot_s_label_is_ignored_without_a_role_lookup(self):
        due, _, calls, _ = self.decide({**listing(issue(12)), **routes_for(12, SHA_A, labelled(("Bot", "claude[bot]")))})
        self.assertFalse(due)
        self.assertFalse([c for c in calls if "/collaborators/" in c])

    def test_the_last_application_decides(self):
        # Removed and applied again: the label in force is the latest one.
        due, _, _, _ = self.decide(
            {
                **listing(issue(12)),
                **routes_for(12, SHA_A, labelled(("User", "vitali87"), ("Bot", "claude[bot]")), {"vitali87": "admin"}),
            }
        )
        self.assertFalse(due)

    def test_a_label_with_no_recorded_application_is_ignored(self):
        due, lines, _, _ = self.decide({**listing(issue(12)), **routes_for(12, SHA_A, labelled())})
        self.assertFalse(due)
        self.assertIn("'no recorded event'", lines[0])

    def test_a_login_that_is_not_one_reaches_no_url(self):
        due, _, calls, _ = self.decide({**listing(issue(12)), **routes_for(12, SHA_A, labelled(("User", "../x")))})
        self.assertFalse(due)
        self.assertFalse([c for c in calls if "/collaborators/" in c])

    def test_a_merge_outside_the_unreleased_versions_does_not_count(self):
        # Already published, or merged after head's version was cut.
        due, _, calls, _ = self.decide(
            {**listing(issue(12)), **routes_for(12, SHA_OUT, labelled(("User", "vitali87")), {"vitali87": "admin"})}
        )
        self.assertFalse(due)
        self.assertFalse([c for c in calls if "/events" in c])

    def test_an_unmerged_pull_request_or_an_issue_is_not_looked_up(self):
        due, _, calls, _ = self.decide(listing(issue(12, merged=False), issue(13, pull=False)))
        self.assertFalse(due)
        self.assertEqual(len(calls), 1)

    def test_only_the_exact_label_counts(self):
        due, _, calls, _ = self.decide(listing(issue(12, labels=("release", "release nowx"))))
        self.assertFalse(due)
        self.assertEqual(len(calls), 1)

    def test_a_number_that_is_not_one_reaches_no_url(self):
        due, _, calls, _ = self.decide(listing(issue("1/../x"), issue(0), issue(True)))
        self.assertFalse(due)
        self.assertEqual(len(calls), 1)

    def test_every_page_of_the_listing_is_read(self):
        first = f"{API}issues?labels=release%20now&state=closed&{SINCE}&per_page=100"
        second = f"{API}issues?labels=release%20now&state=closed&page=2"
        routes = {
            first: ([issue(11, merged=False)], f'<{second}>; rel="next", <{second}>; rel="last"'),
            second: [issue(12)],
            **routes_for(12, SHA_A, labelled(("User", "vitali87")), {"vitali87": "admin"}),
        }
        due, _, calls, _ = self.decide(routes)
        self.assertTrue(due)
        self.assertEqual(calls[:2], [first, second])

    def test_a_next_page_off_the_api_is_refused(self):
        first = f"{API}issues?labels=release%20now&state=closed&{SINCE}&per_page=100"
        routes = {first: ([], '<https://elsewhere.example/steal>; rel="next"')}
        with self.assertRaisesRegex(rn.ReleaseNowError, "outside"):
            self.decide(routes)

    def test_a_failed_lookup_raises_instead_of_guessing(self):
        base = {**listing(issue(12)), **routes_for(12, SHA_A, labelled(("User", "vitali87")), {"vitali87": "admin"})}
        for url in base:
            with self.subTest(url=url):
                with self.assertRaisesRegex(rn.ReleaseNowError, "failed"):
                    self.decide(base, fail=[url])

    def test_nothing_unreleased_makes_no_call(self):
        due, _, calls, _ = self.decide({}, commits=())
        self.assertFalse(due)
        self.assertEqual(calls, [])


@unittest.skipUnless(shutil.which("git"), "needs git")
class Cli(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)
        self.main = Main(self.dir)
        self.main.merge("a")
        self.merge = git(self.dir, "rev-parse", "HEAD")
        self.main.release("0.2.12")
        self.summary = self.dir / "summary.md"

    def run_main(self, routes, fail=()):
        env = {
            "GH_TOKEN": "t0ken",
            "GITHUB_REPOSITORY": "vitali87/croft",
            "GITHUB_API_URL": "https://api.example",
            "GITHUB_STEP_SUMMARY": str(self.summary),
        }
        cwd = os.getcwd()
        os.chdir(self.dir)
        self.addCleanup(os.chdir, cwd)
        with mock.patch.dict(os.environ, env), mock.patch.object(rn, "fetch", FakeApi(routes, fail)):
            with redirect_stdout(io.StringIO()) as out, redirect_stderr(io.StringIO()) as err:
                code = rn.main(["0.2.11"])
        return code, out.getvalue(), err.getvalue()

    def test_prints_the_verdict_and_writes_the_summary(self):
        code, out, err = self.run_main(
            {**listing(issue(12)), **routes_for(12, self.merge, labelled(("User", "vitali87")), {"vitali87": "admin"})}
        )
        self.assertEqual(code, 0, err)
        self.assertEqual(out, "release_now=true\n")
        self.assertIn("#12, labelled by @vitali87 (admin)", self.summary.read_text())

    def test_no_label_is_a_no(self):
        code, out, err = self.run_main(listing())
        self.assertEqual(code, 0, err)
        self.assertEqual(out, "release_now=false\n")
        self.assertFalse(self.summary.exists())

    def test_a_failed_lookup_refuses_with_nothing_on_stdout(self):
        url = next(iter(listing()))
        code, out, err = self.run_main(listing(), fail=[url])
        self.assertEqual(code, 2)
        self.assertEqual(out, "")
        self.assertIn("::error::release_now: GET", err)

    def test_a_malformed_version_is_refused_by_the_script(self):
        proc = subprocess.run(
            [sys.executable, str(CLI), "0.2.x"], cwd=self.dir, capture_output=True, text=True, check=False
        )
        self.assertEqual(proc.returncode, 2)
        self.assertEqual(proc.stdout, "")
        self.assertIn("malformed version", proc.stderr)


if __name__ == "__main__":
    unittest.main()
