#!/usr/bin/env python3
"""Vet an unfamiliar GitHub contributor before a maintainer spends time on them.

The deterministic half of the vet-contributor skill (../SKILL.md). It answers
two questions and keeps them apart:

  TRUST  Does the author's record across GitHub look like a person who
         finishes what they open? Merge rate elsewhere, PR velocity, a quiet
         account that suddenly opens PRs at volume, spam labels from other
         maintainers, badge farming, scheduler-like timing, silence after
         review.
  RISK   What could this pull request do if the answer to TRUST is wrong?
         CI, build, dependency and agent-config paths, hidden text, prompt
         injection aimed at AI reviewers, exec and network calls.

A good TRUST never lowers RISK: reputation is what a patient attacker builds
first, and accounts are bought, reactivated and farmed.

Two subcommands:

  collect  Fetch the signals for one pull request's author with `gh api` and
           write them as JSON. A section gh cannot fetch is listed under
           `gaps` instead of failing the run, so the file can be completed by
           hand (../reference.md maps every field to a GitHub MCP query).
  score    Turn a signals file into the TRUST tier, the RISK level, and the
           recommended next step, as Markdown or JSON.

Standard library only; `collect` needs an authenticated `gh` on PATH.
"""

# ruff: noqa: T201
from __future__ import annotations

import argparse
import base64
import json
import re
import subprocess
import sys
from dataclasses import asdict, dataclass, field
from datetime import UTC, datetime, timedelta
from pathlib import Path

SCHEMA = 1

BASE_SCORE = 50
TIER_HIGH = 70
TIER_MEDIUM = 45
TIER_LOW = 25
THIN_RECORD = 3
MIN_ACCOUNT_AGE_DAYS = 30
REVIEW_SILENCE_DAYS = 7
MAX_LOC = 400
MAX_FILES = 20
SAMPLE = 100
CADENCE_WINDOW = 20
CADENCE_MIN_SAMPLE = 10
CADENCE_SHARE = 0.6
# Merge rate on other people's repos: (floor, points), best first; below the
# last floor the record costs 20.
MERGE_RATE_POINTS = ((0.8, 20), (0.6, 10), (0.4, 0), (0.2, -10))

TIER_ORDER = ("HIGH", "MEDIUM", "LOW", "VERY LOW")
SHORT_CIRCUIT_TIERS = ("BLOCKED", "BOT", "VOUCHED", "RETURNING")

# Hacktoberfest's labels for PRs that do not count; other maintainers apply
# them to PRs they consider spam.
SPAM_LABELS = ("spam", "invalid")

# GitHub achievements a farm can unlock from its own repositories.
ACHIEVEMENT_REPO = re.compile(
    r"pair[-_ ]?extraordinaire|pull[-_ ]?shark|quick[-_ ]?draw|\byolo\b"
    r"|galaxy[-_ ]?brain|starstruck|public[-_ ]?sponsor|achievement",
    re.IGNORECASE,
)

# Paths where a change runs code on a maintainer's machine, in CI, at install
# or test-collection time, or steers the agents that review and implement.
HIGH_PATHS = (
    (
        "ci-or-automation",
        re.compile(
            r"^\.github/|^\.gitlab-ci\.ya?ml$|^\.circleci/|(^|/)Jenkinsfile$"
            r"|^azure-pipelines|^\.buildkite/"
        ),
        "runs in CI with the repository's token and secrets",
    ),
    (
        "agent-instructions",
        re.compile(
            r"(^|/)\.claude/|(^|/)(CLAUDE|AGENTS|GEMINI)\.md$|(^|/)\.cursor(/|rules)"
            r"|(^|/)\.mcp\.json$|(^|/)greptile\.json$|(^|/)\.coderabbit\.ya?ml$"
        ),
        "steers the agents and review bots that read this repository",
    ),
    (
        "build-or-packaging",
        re.compile(
            r"(^|/)(setup\.py|setup\.cfg|pyproject\.toml|MANIFEST\.in|Makefile"
            r"|GNUmakefile|CMakeLists\.txt|build\.rs|Cargo\.toml|package\.json"
            r"|noxfile\.py|tox\.ini|[Jj]ustfile|Taskfile\.ya?ml|Dockerfile[^/]*"
            r"|docker-compose[^/]*\.ya?ml|compose\.ya?ml|rust-toolchain(\.toml)?)$"
            r"|\.spec$"
        ),
        "runs at build or install time",
    ),
    (
        "dependencies",
        re.compile(
            r"(^|/)(uv\.lock|poetry\.lock|Pipfile(\.lock)?|Cargo\.lock"
            r"|package-lock\.json|npm-shrinkwrap\.json|yarn\.lock|pnpm-lock\.yaml"
            r"|go\.(mod|sum)|Gemfile(\.lock)?|requirements[^/]*\.(txt|in))$"
        ),
        "changes what gets downloaded and executed",
    ),
    (
        "tooling-that-runs-code",
        re.compile(
            r"(^|/)(conftest\.py|sitecustomize\.py|usercustomize\.py"
            r"|\.pre-commit-config\.ya?ml|\.envrc|\.gitmodules|\.gitattributes"
            r"|\.npmrc|\.pypirc|pip\.conf)$|(^|/)\.vscode/|(^|/)\.devcontainer/"
            r"|(^|/)\.idea/"
        ),
        "runs automatically in a contributor's or maintainer's tooling",
    ),
)
ELEVATED_PATHS = (
    (
        "scripts",
        re.compile(r"\.(sh|bash|zsh|ps1|bat|cmd)$|(^|/)scripts?/"),
        "a script someone will run",
    ),
    (
        "project-policy",
        re.compile(r"(^|/)(SECURITY|LICENSE|CODEOWNERS|GOVERNANCE)(\.md|\.txt)?$"),
        "changes who decides or how issues are reported",
    ),
)
BINARY_EXTENSIONS = (
    ".so", ".dll", ".dylib", ".exe", ".bin", ".whl", ".jar", ".pyc", ".pyd",
    ".node", ".wasm", ".zip", ".tar", ".gz", ".tgz", ".xz", ".7z", ".a", ".o",
)  # fmt: skip
MEDIA_EXTENSIONS = (
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".ico", ".mp4", ".webm",
)  # fmt: skip
TEST_PATH = re.compile(r"(^|/)(tests?|__tests__|spec)/|(^|/)test_[^/]+$|_test\.\w+$")

# Hidden-content carriers, as in croft's issue vetting: zero-width and bidi
# characters hide or reorder text (Trojan Source); a long base64 run is an
# opaque payload; pipe-to-shell and CI token names are what a contribution
# never legitimately needs.
ZERO_WIDTH = re.compile("[\u200b-\u200f\u2060\ufeff]")
BIDI_CONTROLS = re.compile("[\u202a-\u202e\u2066-\u2069]")
BASE64_BLOB = re.compile(r"[A-Za-z0-9+/=]{200,}")
PIPE_TO_SHELL = re.compile(r"\b(?:curl|wget)\b[^\n|]*\|\s*(?:sudo\s+)?(?:ba|z|da)?sh\b")
CI_SECRET = re.compile(
    r"\bsecrets\.[A-Z_]+|\bGITHUB_TOKEN\b|\bGH_TOKEN\b|\bACTIONS_RUNTIME_TOKEN\b"
    r"|\bACTIONS_ID_TOKEN_REQUEST_(?:URL|TOKEN)\b"
)
HTML_COMMENT = re.compile(r"<!--(.*?)-->", re.DOTALL)
# Review bots write short markers into PR bodies (CodeRabbit's release-notes
# block); those are not the author hiding text. Injection inside any comment
# is still caught by the prose scan, which reads the raw body.
BOT_MARKER = re.compile(
    r"auto-generated|coderabbit|greptile|copilot|sourcery|ellipsis|graphite|codecov",
    re.IGNORECASE,
)
BOT_MARKER_MAX = 120
# Editors prepend a byte-order mark on paste; only one elsewhere hides text.
BOM = "\ufeff"

# Text addressed to an AI reviewer. Code in an LLM project legitimately says
# "system prompt" or "you are an assistant", so added lines are held to the
# narrower pattern and prose (title, body, commit messages) to the full one.
INJECTION_ANYWHERE = re.compile(
    r"ignore\s+(?:all\s+|any\s+|the\s+)?(?:previous|prior|above|earlier)\s+"
    r"(?:instructions|prompts|rules|messages)"
    r"|disregard\s+(?:all\s+|any\s+|the\s+)?(?:previous|prior|above|earlier)\b"
    r"|(?:note|message|instructions?)\s+(?:to|for)\s+(?:the\s+)?(?:ai|llm|bots?|claude"
    r"|copilot|automated)\b"
    r"|(?:ai|llm|automated|bot)\s+(?:code\s+)?reviewers?\s*(?:should|must|:)"
    r"|approve\s+this\s+(?:pr|pull\s+request|change)",
    re.IGNORECASE,
)
INJECTION_PROSE = re.compile(
    INJECTION_ANYWHERE.pattern
    + r"|\bsystem\s+prompt\b|you\s+are\s+(?:now\s+)?(?:an?\s+)?(?:ai|llm|language\s+model)\b"
    r"|</?(?:system|instructions?|assistant)>",
    re.IGNORECASE,
)

EXEC_OR_NETWORK = re.compile(
    r"\bsubprocess\b|\bos\.(?:system|popen|exec\w*|spawn\w*)\b|\beval\s*\(|\bexec\s*\("
    r"|\b__import__\s*\(|\bpickle\.loads?\b|\bmarshal\.loads\b|\bctypes\b"
    r"|\bsocket\.\w+|\burllib\.request\b|\brequests\.(?:get|post|put|request)\b"
    r"|\bhttpx\.\w+|\baiohttp\b|\bb64decode\b|\bCommand::new\b|\bstd::process\b"
    r"|\bunsafe\s*\{|\bchild_process\b|\bfetch\s*\(\s*[\"'`]https?:|\bXMLHttpRequest\b"
)
TEST_WEAKENING = re.compile(
    r"pytest\.mark\.(?:skip|xfail)|@unittest\.skip|#\[ignore\]|\b(?:it|describe|test)\.skip\("
)
HUNK_HEADER = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@")


@dataclass
class Finding:
    points: int
    reason: str


@dataclass
class Trust:
    tier: str
    score: int
    findings: list[Finding] = field(default_factory=list)
    notes: list[str] = field(default_factory=list)
    gaps: list[str] = field(default_factory=list)


@dataclass
class RiskFlag:
    level: str
    kind: str
    where: str
    detail: str


@dataclass
class Risk:
    level: str
    flags: list[RiskFlag] = field(default_factory=list)


@dataclass
class TrustList:
    vouched: set[str] = field(default_factory=set)
    denounced: dict[str, str] = field(default_factory=dict)


def parse_time(value: object) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        stamp = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return stamp if stamp.tzinfo else stamp.replace(tzinfo=UTC)


def _int(value: object) -> int | None:
    # A bool is an int subclass; a count that arrives as true/false is unknown.
    if isinstance(value, bool) or not isinstance(value, int):
        return None
    return value


def parse_trust_list(text: str) -> TrustList:
    """Read a Vouch trust file (VOUCHED.td): `user`, `-user reason`, `# comment`."""
    trust = TrustList()
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        denounce = line.startswith("-")
        handle, _, reason = line.lstrip("-").strip().partition(" ")
        platform, _, name = handle.rpartition(":")
        if platform and platform.lower() != "github":
            continue
        name = name.lstrip("@").lower()
        if not name:
            continue
        if denounce:
            trust.denounced[name] = reason.strip()
        else:
            trust.vouched.add(name)
    return trust


def cadence(timestamps: list[str]) -> tuple[str, int, int] | None:
    """Find a scheduler's beat in the most recent creation times.

    A person opens PRs at any minute and second. A scheduler fires on a fixed
    beat and its API calls land shortly after: a minute divisible by five
    (chance: 1 in 5), or the first 10 seconds of a minute (chance: 1 in 6).
    Only the latest events count, because an agent is often switched on
    partway through an account's history. Returns the beat, its hits and the
    sample size, or None when the sample is too small or shows no beat.
    """
    stamps = sorted((s for s in (parse_time(t) for t in timestamps) if s), reverse=True)
    stamps = stamps[:CADENCE_WINDOW]
    if len(stamps) < CADENCE_MIN_SAMPLE:
        return None
    beats = (
        (
            "a minute divisible by five (chance: 1 in 5)",
            sum(1 for s in stamps if s.minute % 5 == 0),
        ),
        (
            "the first 10 seconds of a minute (chance: 1 in 6)",
            sum(1 for s in stamps if s.second < 10),
        ),
    )
    beat, hits = max(beats, key=lambda b: b[1])
    return (beat, hits, len(stamps)) if hits >= CADENCE_SHARE * len(stamps) else None


def merge_rate_points(rate: float) -> int:
    return next((points for floor, points in MERGE_RATE_POINTS if rate >= floor), -20)


def is_bot(login: str, account: dict) -> bool:
    return login.endswith("[bot]") or account.get("type") == "Bot"


def commit_strangers(pr: dict, author: str, owner: str) -> list[str]:
    """Logins other than the PR author that authored non-merge commits."""
    strangers: list[str] = []
    for commit in pr.get("commits") or []:
        if (_int(commit.get("parents")) or 1) > 1:
            continue
        who = str(commit.get("author_login") or "")
        if (
            who
            and who.lower() not in {author.lower(), owner.lower()}
            and who not in strangers
        ):
            strangers.append(who)
    return strangers


def score_trust(
    signals: dict, now: datetime, trust_list: TrustList | None = None
) -> Trust:
    login = str(signals.get("login") or "")
    owner = str(signals.get("repo") or "").partition("/")[0]
    account = signals.get("account") or {}
    history = signals.get("history") or {}
    repos = signals.get("repos") or {}
    here = signals.get("here") or {}
    pr = signals.get("pr") or {}
    trust_list = trust_list or TrustList()
    findings: list[Finding] = []
    notes: list[str] = []
    gaps = [str(g) for g in signals.get("gaps") or []]

    def add(points: int, reason: str) -> None:
        findings.append(Finding(points, reason))

    created = parse_time(account.get("created_at"))
    if created is None:
        gaps.append("account creation date unknown")
    else:
        days = (now - created).days
        if days < MIN_ACCOUNT_AGE_DAYS:
            add(-25, f"account is {days} days old")
        elif days < 365:
            add(
                -10,
                f"account is {days} days old; fresh accounts merge ~16 points less often (Good Egg)",
            )
        elif days >= 3 * 365:
            add(5, f"account is {days // 365} years old")
    followers = _int(account.get("followers"))
    if followers is not None and followers >= 100:
        add(5, f"{followers} followers")
    if _int(account.get("profile_fields")) == 0 and followers == 0:
        add(-5, "empty profile and no followers")

    merged = _int(history.get("external_merged"))
    closed = _int(history.get("external_closed_unmerged"))
    still_open = _int(history.get("external_open"))
    decided = None
    if merged is None or closed is None:
        gaps.append("record on other people's repos unknown")
    else:
        decided = merged + closed
        if decided < THIN_RECORD:
            notes.append(
                f"only {decided} decided PRs on other people's repos: no track record to judge"
            )
        else:
            rate = merged / decided
            add(
                merge_rate_points(rate),
                f"{merged} of {decided} decided PRs on other people's repos merged ({rate:.0%})",
            )

    lifetime = None if decided is None or still_open is None else decided + still_open
    prs_60 = _int(history.get("external_prs_60d"))
    first = parse_time(history.get("first_external_pr_at"))
    if lifetime and prs_60 is not None and first is not None:
        if (now - first).days >= 365 and prs_60 * 2 < lifetime:
            add(
                10,
                f"contributing to other projects since {first:%Y-%m}, mostly before the last 60 days",
            )
    if (
        lifetime is not None
        and prs_60 is not None
        and lifetime >= 20
        and prs_60 >= 0.8 * lifetime
    ):
        add(
            -10,
            f"{prs_60} of {lifetime} lifetime PRs to other repos opened in the last 60 days: a quiet account that suddenly opens PRs at volume",
        )

    spam = _int(history.get("spam_labelled"))
    if spam:
        add(-20, f"{spam} PRs labelled spam or invalid by other maintainers")

    prs_30 = _int(history.get("external_prs_30d"))
    repos_30 = _int(history.get("external_repos_30d")) or 0
    if prs_30 is None:
        gaps.append("PR velocity unknown")
    elif prs_30 >= 60:
        add(
            -30,
            f"{prs_30} PRs to other people's repos in the last 30 days ({repos_30}+ repos)",
        )
    elif prs_30 >= 30 and repos_30 >= 15:
        add(-20, f"{prs_30} PRs across {repos_30}+ repos in the last 30 days")
    elif prs_30 >= 15:
        add(-5, f"{prs_30} PRs to other people's repos in the last 30 days")

    forks_24h = _int(repos.get("max_forks_24h"))
    if forks_24h is not None and forks_24h >= 6:
        add(-10, f"forked {forks_24h} repos within 24 hours")
    originals_30 = _int(repos.get("originals_30d"))
    if originals_30 is not None and originals_30 >= 20:
        add(-5, f"created {originals_30} repos of their own in the last 30 days")
    achievement = [str(n) for n in repos.get("achievement_named") or []]
    if achievement:
        add(
            -10,
            f"repos named after GitHub achievements ({', '.join(achievement[:3])}): badge farming",
        )
    fast = _int(history.get("fast_self_merges"))
    if fast is not None and fast >= 5:
        add(
            -10,
            f"{fast} PRs in their own repos merged within a minute of opening: contribution-count farming",
        )

    beat = cadence([str(t) for t in signals.get("timestamps") or []])
    if beat:
        where, hits, sampled = beat
        add(
            -20,
            f"{hits} of the latest {sampled} PR and repo creation times fall in {where}: a scheduler, not a person",
        )

    here_merged = _int(here.get("merged")) or 0
    here_closed = _int(here.get("closed_unmerged")) or 0
    if here_closed >= 2 and here_merged == 0:
        add(-10, f"{here_closed} earlier PRs to this repo closed unmerged")

    fork = pr.get("head_repo") and pr.get("head_repo") != signals.get("repo")
    if fork and pr.get("maintainer_can_modify") is False:
        add(-5, "maintainers cannot push to the PR branch")
    strangers = commit_strangers(pr, login, owner)
    if strangers:
        add(-10, f"commits authored by other accounts: {', '.join(strangers[:3])}")
    if any(c.get("author_login") is None for c in pr.get("commits") or []):
        notes.append("some commits use an email linked to no GitHub account")
    first_review = parse_time(pr.get("first_human_review_at"))
    if first_review is not None and pr.get("author_replied_after_review") is False:
        silent = (now - first_review).days
        if silent >= REVIEW_SILENCE_DAYS:
            add(-10, f"no reply in the {silent} days since the first human review")
        else:
            notes.append(f"no reply yet to human review ({silent} days)")

    fork_made = parse_time(pr.get("head_repo_created_at"))
    first_commit = min(
        (
            s
            for s in (parse_time(c.get("authored_at")) for c in pr.get("commits") or [])
            if s
        ),
        default=None,
    )
    if (
        fork_made
        and first_commit
        and timedelta(0) <= first_commit - fork_made < timedelta(minutes=2)
    ):
        loc = (_int(pr.get("additions")) or 0) + (_int(pr.get("deletions")) or 0)
        notes.append(
            f"first commit authored {int((first_commit - fork_made).total_seconds())} s after the fork was created ({loc} changed lines)"
        )

    score = max(0, min(100, BASE_SCORE + sum(f.points for f in findings)))
    key = login.lower()
    if key in trust_list.denounced:
        reason = trust_list.denounced[key]
        tier = "BLOCKED"
        notes.insert(
            0, "denounced in the trust file" + (f": {reason}" if reason else "")
        )
    elif is_bot(login, account):
        tier = "BOT"
    elif key in trust_list.vouched:
        tier = "VOUCHED"
    elif here_merged:
        tier = "RETURNING"
        notes.insert(0, f"{here_merged} PRs already merged in this repo")
    elif (
        decided is not None
        and decided < THIN_RECORD
        and all(f.points >= 0 for f in findings)
    ):
        tier = "UNKNOWN"
    elif score >= TIER_HIGH:
        tier = "HIGH"
    elif score >= TIER_MEDIUM:
        tier = "MEDIUM"
    elif score >= TIER_LOW:
        tier = "LOW"
    else:
        tier = "VERY LOW"
    if tier == "HIGH" and gaps:
        tier = "MEDIUM"
        notes.append("capped at MEDIUM: some signals could not be collected")
    return Trust(tier, score, findings, notes, gaps)


def added_lines(patch: str) -> list[tuple[int, str]]:
    """The added lines of a unified diff, with their line numbers in the new file."""
    lines: list[tuple[int, str]] = []
    number = 0
    for raw in patch.splitlines():
        header = HUNK_HEADER.match(raw)
        if header:
            number = int(header.group(1))
            continue
        if raw.startswith("+") and not raw.startswith("+++"):
            lines.append((number, raw[1:]))
            number += 1
        elif not raw.startswith("-"):
            number += 1
    return lines


def _hidden(text: str) -> list[tuple[str, str]]:
    text = text.removeprefix(BOM)
    found = []
    if ZERO_WIDTH.search(text):
        found.append(("zero-width-characters", "invisible characters that hide text"))
    if BIDI_CONTROLS.search(text):
        found.append(
            (
                "bidi-controls",
                "bidirectional controls that reorder what a reviewer sees (Trojan Source)",
            )
        )
    if PIPE_TO_SHELL.search(text):
        found.append(("pipe-to-shell", "downloads and runs a remote script"))
    if CI_SECRET.search(text):
        found.append(("ci-secret-reference", "names a CI token or secret"))
    return found


def score_risk(pr: dict) -> Risk:
    if not pr:
        return Risk("n/a")
    files = pr.get("files")
    if not files:
        return Risk(
            "unknown",
            [
                RiskFlag(
                    "elevated",
                    "no-file-list",
                    "PR",
                    "the changed files were not collected; read the diff yourself",
                )
            ],
        )

    grouped: dict[tuple[str, str, str], list[int]] = {}
    details: dict[tuple[str, str, str], str] = {}

    def flag(
        level: str, kind: str, where: str, detail: str, line: int | None = None
    ) -> None:
        key = (level, kind, where)
        grouped.setdefault(key, [])
        details.setdefault(key, detail)
        if line is not None:
            grouped[key].append(line)

    for entry in files:
        name = str(entry.get("filename") or "")
        status = str(entry.get("status") or "")
        for kind, pattern, why in HIGH_PATHS:
            if pattern.search(name):
                flag("high", kind, name, why)
        for kind, pattern, why in ELEVATED_PATHS:
            if pattern.search(name):
                flag("elevated", kind, name, why)
        if status == "removed" and TEST_PATH.search(name):
            flag("elevated", "test-removed", name, "a test file is deleted")
        patch = entry.get("patch")
        if not isinstance(patch, str):
            lower = name.lower()
            if lower.endswith(BINARY_EXTENSIONS):
                flag(
                    "high",
                    "binary",
                    name,
                    "a compiled or archived file nobody can review as text",
                )
            elif status != "removed" and not lower.endswith(MEDIA_EXTENSIONS):
                flag(
                    "elevated",
                    "no-diff",
                    name,
                    "the API returned no diff (binary or too large); read it locally",
                )
            continue
        for number, text in added_lines(patch):
            for kind, why in _hidden(text):
                flag("high", kind, name, why, number)
            if BASE64_BLOB.search(text):
                flag(
                    "high", "base64-blob", name, "a long opaque encoded payload", number
                )
            if INJECTION_ANYWHERE.search(text):
                flag(
                    "high",
                    "prompt-injection",
                    name,
                    "text addressed to an AI reviewer or agent",
                    number,
                )
            if EXEC_OR_NETWORK.search(text):
                flag(
                    "elevated",
                    "exec-or-network",
                    name,
                    "runs a process, evaluates code, deserializes, or opens a connection",
                    number,
                )
            if TEST_WEAKENING.search(text):
                flag(
                    "elevated",
                    "test-weakening",
                    name,
                    "skips or expects a test to fail",
                    number,
                )

    template_comments = {
        c.strip() for c in HTML_COMMENT.findall(str(pr.get("template") or ""))
    }
    prose = [
        ("PR title", str(pr.get("title") or "")),
        ("PR body", str(pr.get("body") or "")),
    ]
    for commit in pr.get("commits") or []:
        prose.append(
            (
                f"commit {str(commit.get('sha') or '')[:7]} message",
                str(commit.get("message") or ""),
            )
        )
    for where, text in prose:
        for kind, why in _hidden(text):
            flag("high", kind, where, why)
        if INJECTION_PROSE.search(text):
            flag(
                "high",
                "prompt-injection",
                where,
                "text addressed to an AI reviewer or agent",
            )
        hidden_notes = [
            c
            for c in HTML_COMMENT.findall(text)
            if c.strip()
            and c.strip() not in template_comments
            and not (len(c) <= BOT_MARKER_MAX and BOT_MARKER.search(c))
        ]
        if hidden_notes:
            flag(
                "elevated",
                "hidden-html-comment",
                where,
                "an HTML comment the PR template does not have: invisible on GitHub, read verbatim by AI reviewers",
            )

    loc = (_int(pr.get("additions")) or 0) + (_int(pr.get("deletions")) or 0)
    changed = _int(pr.get("changed_files")) or len(files)
    if loc > MAX_LOC or changed > MAX_FILES:
        flag(
            "elevated",
            "size",
            "PR",
            f"{loc} changed lines in {changed} files; past {MAX_LOC} lines review quality drops",
        )

    flags = []
    for (level, kind, where), numbers in grouped.items():
        place = where
        if numbers:
            shown = ", ".join(str(n) for n in numbers[:5])
            more = f" (+{len(numbers) - 5} more)" if len(numbers) > 5 else ""
            place = f"{where}:{shown}{more}"
        flags.append(RiskFlag(level, kind, place, details[(level, kind, where)]))
    flags.sort(key=lambda f: (f.level != "high", f.kind, f.where))
    level = (
        "high"
        if any(f.level == "high" for f in flags)
        else "elevated"
        if flags
        else "low"
    )
    return Risk(level, flags)


def recommend(trust: Trust, risk: Risk) -> list[str]:
    if trust.tier == "BLOCKED":
        return ["Close it: the author is denounced in the trust file."]
    steps: list[str] = []
    if risk.level == "high":
        steps.append(
            "Read every HIGH-flagged file in full before you approve this fork's workflow "
            "runs, whatever the trust tier: approving runs the PR's code on your runners."
        )
        steps.append(
            "Keep agents and review bots off this PR's text until you have read the flagged "
            "hunks; the PR is untrusted input to them."
        )
    match trust.tier:
        case "BOT":
            steps.append(
                "Confirm it is an integration you installed, then review as usual."
            )
        case "VOUCHED" | "RETURNING" | "HIGH":
            steps.append("Review normally.")
        case "MEDIUM" | "UNKNOWN":
            steps.append(
                "Review on the merits. Before a deep review, ask one specific question about a "
                "non-obvious choice in the diff; a person answers it specifically."
            )
        case "LOW":
            steps.append(
                "Ask one specific question about the change and wait for the answer before "
                "approving CI or reviewing in depth."
            )
        case _:
            steps.append(
                "Treat it as unattended automation. Spend no review time until a person answers "
                "a specific question; close it after 7 days without one. If the idea is good, "
                "implement it yourself."
            )
    if risk.level == "elevated":
        steps.append("Read the ELEVATED hunks listed below closely.")
    return steps


def render(signals: dict, trust: Trust, risk: Risk) -> str:
    login = signals.get("login") or "?"
    repo = signals.get("repo") or "?"
    number = (signals.get("pr") or {}).get("number")
    target = f"{repo}#{number}" if number else repo
    out = [
        f"## Contributor vetting: @{login} on {target}",
        "",
        f"**Trust: {trust.tier}** ({trust.score}/100), **Risk: {risk.level.upper()}**",
        "",
        "### Next step",
        "",
        *[f"- {step}" for step in recommend(trust, risk)],
        "",
        "### Trust signals",
        "",
    ]
    if trust.findings:
        out += ["| Points | Signal |", "| ---: | --- |"]
        out += [
            f"| {f.points:+d} | {f.reason} |"
            for f in sorted(trust.findings, key=lambda f: f.points)
        ]
    else:
        out.append("No scored signals.")
    if trust.notes:
        out += ["", *[f"- {note}" for note in trust.notes]]
    out += ["", "### Risk flags", ""]
    if risk.flags:
        out += [
            f"- **{f.level.upper()}** `{f.kind}` {f.where}: {f.detail}"
            for f in risk.flags
        ]
    else:
        out.append(
            "None found."
            if risk.level == "low"
            else "Not assessed: no pull request in the signals."
        )
    if trust.gaps:
        out += ["", "### Not collected", "", *[f"- {gap}" for gap in trust.gaps]]
    out += [
        "",
        "Scores rank track records; they do not prove intent. Finish the manual checks in "
        "SKILL.md before acting on this.",
    ]
    return "\n".join(out)


# --- collect -----------------------------------------------------------------


class GhError(RuntimeError):
    pass


def gh_json(path: str, params: dict[str, object] | None = None) -> object:
    command = ["gh", "api", "-X", "GET", path]
    for key, value in (params or {}).items():
        command += ["-f", f"{key}={value}"]
    # Fixed argv, no shell; the arguments are the repository, a number and a
    # login gh itself validates.
    result = subprocess.run(
        command, capture_output=True, text=True, timeout=120, check=False
    )  # noqa: S603
    if result.returncode != 0:
        lines = (result.stderr or result.stdout).strip().splitlines()
        raise GhError(lines[-1] if lines else f"gh api {path} failed")
    return json.loads(result.stdout or "null")


def gh_object(path: str) -> dict:
    result = gh_json(path)
    if not isinstance(result, dict):
        raise GhError(f"gh api {path} returned no object")
    return result


def gh_pages(
    path: str, params: dict[str, object] | None = None, max_pages: int = 10
) -> list[dict]:
    items: list[dict] = []
    for page in range(1, max_pages + 1):
        batch = gh_json(path, {**(params or {}), "per_page": 100, "page": page})
        if not isinstance(batch, list):
            break
        items += [b for b in batch if isinstance(b, dict)]
        if len(batch) < 100:
            break
    return items


def search_prs(query: str, per_page: int = 1, order: str | None = None) -> dict:
    params: dict[str, object] = {"q": f"type:pr {query}", "per_page": per_page}
    if order:
        params |= {"sort": "created", "order": order}
    result = gh_json("search/issues", params)
    return result if isinstance(result, dict) else {}


def _repo_of(item: dict) -> str:
    return str(item.get("repository_url") or "").removeprefix(
        "https://api.github.com/repos/"
    )


def _who(entry: dict) -> tuple[str, bool]:
    user = entry.get("user") or {}
    login = str(user.get("login") or "")
    return login, login.endswith("[bot]") or user.get("type") == "Bot"


def max_in_window(stamps: list[datetime], window: timedelta) -> int:
    stamps = sorted(stamps)
    best = start = 0
    for end, stamp in enumerate(stamps):
        while stamp - stamps[start] > window:
            start += 1
        best = max(best, end - start + 1)
    return best


def collect(repo: str, number: int, now: datetime) -> dict:
    signals: dict = {
        "schema": SCHEMA,
        "collected_at": now.isoformat(),
        "repo": repo,
        "gaps": [],
    }
    timestamps: list[str] = []

    def section(name: str, fetch) -> None:
        try:
            fetch()
        except (GhError, json.JSONDecodeError, subprocess.TimeoutExpired) as error:
            signals["gaps"].append(f"{name}: {error}")

    def fetch_pr() -> None:
        pull = gh_object(f"repos/{repo}/pulls/{number}")
        author = str((pull.get("user") or {}).get("login") or "")
        signals["login"] = author
        head = (pull.get("head") or {}).get("repo") or {}
        pr = {
            "number": number,
            "title": pull.get("title"),
            "body": pull.get("body"),
            "created_at": pull.get("created_at"),
            "maintainer_can_modify": pull.get("maintainer_can_modify"),
            "additions": pull.get("additions"),
            "deletions": pull.get("deletions"),
            "changed_files": pull.get("changed_files"),
            "head_repo": head.get("full_name"),
            "head_repo_created_at": head.get("created_at"),
        }
        pr["files"] = [
            {
                k: f.get(k)
                for k in ("filename", "status", "additions", "deletions", "patch")
            }
            for f in gh_pages(f"repos/{repo}/pulls/{number}/files", max_pages=30)
        ]
        pr["commits"] = [
            {
                "sha": c.get("sha"),
                "author_login": (c.get("author") or {}).get("login"),
                "author_email": ((c.get("commit") or {}).get("author") or {}).get(
                    "email"
                ),
                "authored_at": ((c.get("commit") or {}).get("author") or {}).get(
                    "date"
                ),
                "message": (c.get("commit") or {}).get("message"),
                "parents": len(c.get("parents") or []),
            }
            for c in gh_pages(f"repos/{repo}/pulls/{number}/commits", max_pages=3)
        ]
        events: list[tuple[datetime, str, bool]] = []
        for path, stamp_key in (
            (f"repos/{repo}/pulls/{number}/reviews", "submitted_at"),
            (f"repos/{repo}/pulls/{number}/comments", "created_at"),
            (f"repos/{repo}/issues/{number}/comments", "created_at"),
        ):
            for entry in gh_pages(path, max_pages=5):
                stamp = parse_time(entry.get(stamp_key))
                if stamp:
                    events.append((stamp, *_who(entry)))
        human = [s for s, who, bot in events if who and who != author and not bot]
        if human:
            first = min(human)
            pr["first_human_review_at"] = first.isoformat()
            pr["author_replied_after_review"] = any(
                s > first and who == author for s, who, _ in events
            )
        for candidate in (
            ".github/pull_request_template.md",
            ".github/PULL_REQUEST_TEMPLATE.md",
            "PULL_REQUEST_TEMPLATE.md",
            "docs/pull_request_template.md",
        ):
            try:
                content = gh_json(f"repos/{repo}/contents/{candidate}")
            except GhError:
                continue
            if isinstance(content, dict) and content.get("content"):
                pr["template"] = base64.b64decode(content["content"]).decode(
                    "utf-8", "replace"
                )
                break
        signals["pr"] = pr

    section("pull request", fetch_pr)
    login = signals.get("login")
    if not login:
        return signals

    def fetch_account() -> None:
        user = gh_object(f"users/{login}")
        profile = (
            "name",
            "bio",
            "company",
            "blog",
            "location",
            "email",
            "twitter_username",
        )
        signals["account"] = {
            "type": user.get("type"),
            "created_at": user.get("created_at"),
            "followers": user.get("followers"),
            "public_repos": user.get("public_repos"),
            "profile_fields": sum(1 for key in profile if user.get(key)),
        }

    def fetch_history() -> None:
        outside = f"author:{login} -user:{login}"
        merged = search_prs(f"{outside} is:merged", per_page=SAMPLE)
        recent = search_prs(
            f"{outside} created:>={(now - timedelta(days=30)).date()}", per_page=SAMPLE
        )
        first = search_prs(outside, order="asc").get("items") or []
        own = search_prs(
            f"author:{login} user:{login} is:merged", per_page=SAMPLE, order="desc"
        )
        fast = 0
        for item in own.get("items") or []:
            opened = parse_time(item.get("created_at"))
            done = parse_time(
                (item.get("pull_request") or {}).get("merged_at")
                or item.get("closed_at")
            )
            if opened and done and done - opened < timedelta(minutes=1):
                fast += 1
        signals["history"] = {
            "external_merged": merged.get("total_count"),
            "merged_external_repos": len(
                {_repo_of(i) for i in merged.get("items") or []}
            ),
            "external_closed_unmerged": search_prs(
                f"{outside} is:closed is:unmerged"
            ).get("total_count"),
            "external_open": search_prs(f"{outside} is:open").get("total_count"),
            "first_external_pr_at": first[0].get("created_at") if first else None,
            "external_prs_30d": recent.get("total_count"),
            "external_repos_30d": len({_repo_of(i) for i in recent.get("items") or []}),
            "external_prs_60d": search_prs(
                f"{outside} created:>={(now - timedelta(days=60)).date()}"
            ).get("total_count"),
            "spam_labelled": sum(
                search_prs(f"author:{login} label:{label}").get("total_count") or 0
                for label in SPAM_LABELS
            ),
            "own_repo_merged": own.get("total_count"),
            "fast_self_merges": fast,
        }
        timestamps.extend(str(i.get("created_at")) for i in recent.get("items") or [])

    def fetch_repos() -> None:
        owned = gh_pages(
            f"users/{login}/repos",
            {"type": "owner", "sort": "created", "direction": "desc"},
            max_pages=5,
        )
        month, quarter = now - timedelta(days=30), now - timedelta(days=90)
        made = [(r, parse_time(r.get("created_at"))) for r in owned]
        forks = [s for r, s in made if r.get("fork") and s]
        signals["repos"] = {
            "owned": len(owned),
            "forks": sum(1 for r in owned if r.get("fork")),
            "originals_30d": sum(
                1 for r, s in made if not r.get("fork") and s and s >= month
            ),
            "forks_30d": sum(1 for s in forks if s >= month),
            "max_forks_24h": max_in_window(forks, timedelta(hours=24)),
            "achievement_named": [
                str(r.get("name"))
                for r in owned
                if ACHIEVEMENT_REPO.search(str(r.get("name") or ""))
            ],
        }
        timestamps.extend(
            str(r.get("created_at")) for r, s in made if s and s >= quarter
        )

    def fetch_here() -> None:
        mine = f"author:{login} repo:{repo}"
        signals["here"] = {
            "merged": search_prs(f"{mine} is:merged").get("total_count"),
            "closed_unmerged": search_prs(f"{mine} is:closed is:unmerged").get(
                "total_count"
            ),
            "open": search_prs(f"{mine} is:open").get("total_count"),
        }

    section("account", fetch_account)
    section("history", fetch_history)
    section("repos", fetch_repos)
    section("this repo", fetch_here)
    signals["timestamps"] = timestamps
    return signals


def _find_trust_file(explicit: str | None) -> Path | None:
    if explicit:
        return Path(explicit)
    for candidate in (Path(".github/VOUCHED.td"), Path("VOUCHED.td")):
        if candidate.is_file():
            return candidate
    return None


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = parser.add_subparsers(dest="command", required=True)

    c = sub.add_parser("collect", help="fetch the signals for a PR's author with gh")
    c.add_argument("--repo", required=True, help="OWNER/REPO")
    c.add_argument("--pr", required=True, type=int)
    c.add_argument("--out", required=True)

    s = sub.add_parser("score", help="score a signals file")
    s.add_argument("--signals", required=True)
    s.add_argument(
        "--trust-file",
        help="Vouch trust file (default: .github/VOUCHED.td or VOUCHED.td)",
    )
    s.add_argument("--now", help="ISO time to score against (default: now)")
    s.add_argument("--json", action="store_true", help="print JSON instead of Markdown")

    args = parser.parse_args(argv)
    if args.command == "collect":
        signals = collect(args.repo, args.pr, datetime.now(UTC))
        Path(args.out).write_text(json.dumps(signals, indent=2), encoding="utf-8")
        for gap in signals["gaps"]:
            print(f"gap: {gap}", file=sys.stderr)
        print(f"wrote {args.out} ({len(signals['gaps'])} gaps)")
        return 0

    try:
        signals = json.loads(Path(args.signals).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        print(f"cannot read signals: {error}", file=sys.stderr)
        return 2
    if not isinstance(signals, dict):
        print("signals must be a JSON object", file=sys.stderr)
        return 2
    now = parse_time(args.now) if args.now else datetime.now(UTC)
    if now is None:
        print(f"--now is not an ISO time: {args.now}", file=sys.stderr)
        return 2
    trust_file = _find_trust_file(args.trust_file)
    trust_list = (
        parse_trust_list(trust_file.read_text(encoding="utf-8"))
        if trust_file and trust_file.is_file()
        else None
    )
    trust = score_trust(signals, now, trust_list)
    risk = score_risk(signals.get("pr") or {})
    if args.json:
        print(
            json.dumps(
                {
                    "trust": asdict(trust),
                    "risk": asdict(risk),
                    "next_step": recommend(trust, risk),
                },
                indent=2,
            )
        )
    else:
        print(render(signals, trust, risk))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
