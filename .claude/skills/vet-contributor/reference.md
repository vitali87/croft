# vet-contributor reference

## How the trust score works

The score starts at 50. Each signal below adds or subtracts points, and the
total is clamped to 0–100. Tiers: **HIGH** ≥ 70, **MEDIUM** ≥ 45, **LOW** ≥ 25,
**VERY LOW** below that.

Four outcomes skip the score:

- **BLOCKED**: denounced in the trust file.
- **BOT**: a `[bot]` login or `type: Bot`.
- **VOUCHED**: vouched in the trust file.
- **RETURNING**: already has a merged PR in this repo.

**UNKNOWN** means fewer than 3 decided PRs on other people's repos and no
negative signal. That is a newcomer with no record, not a bad one. HIGH is
capped at MEDIUM while any signal is missing.

"Other people's repos" excludes the author's own (`-user:LOGIN` in search).
Self-merged PRs in your own repos are free to farm.

| Signal | Points | Why |
| --- | --- | --- |
| Account < 30 days | −25 | Same gate as croft's issue vetting and anti-slop's default. |
| Account < 1 year | −10 | Fresh accounts merge about 16 points less often (Good Egg validation, 5,129 PRs). |
| Account ≥ 3 years | +5 | Weak: every farm in the calibration set was 1–11 years old. |
| ≥ 100 followers | +5 | Weak and gameable. |
| Empty profile, 0 followers | −5 | Weak. |
| Merge rate elsewhere, ≥ 3 decided: ≥ 80% / ≥ 60% / ≥ 40% / ≥ 20% / lower | +20 / +10 / 0 / −10 / −20 | The strongest single predictor (Good Egg: p < 10⁻¹²; their v3 model uses nothing else). |
| First outside PR ≥ 1 year ago and under half of all PRs in the last 60 days | +10 | An established record, not a fresh burst. |
| ≥ 80% of ≥ 20 lifetime outside PRs opened in the last 60 days | −10 | A quiet account that switched on. All four farms in the calibration set show it. |
| PRs labelled `spam` or `invalid` by other maintainers | −20 | Hacktoberfest's labels for PRs that don't count. |
| Outside PRs in 30 days: ≥ 60 / ≥ 30 across ≥ 15 repos / ≥ 15 | −30 / −20 / −5 | Spray. A documented agent opened 103 PRs across 95 repos in two weeks. |
| ≥ 6 forks within 24 hours | −10 | anti-slop's default. |
| ≥ 20 own repos created in 30 days | −5 | Mass repo creation. |
| Repos named after GitHub achievements | −10 | Badge farming (for example `pair-extraordinaire-showcase`). |
| ≥ 5 own-repo PRs merged within a minute of opening | −10 | Contribution-count farming. |
| ≥ 60% of the latest 20 creation times on a minute divisible by 5, or in a minute's first 10 s | −20 | A scheduler's beat (chance: 20% or 17%). Uses only the latest events, because agents get switched on partway through an account's history. |
| ≥ 2 closed-unmerged PRs here and none merged | −10 | |
| Fork PR with maintainer edits disabled | −5 | anti-slop requires them. |
| Non-merge commits authored by another account | −10 | Merge commits and the repo owner's commits are ignored. |
| No reply ≥ 7 days after the first human review | −10 | Abandonment is the commonest slop symptom. Before 7 days it is only a note. |

Notes that score nothing:

- A commit email not linked to any account.
- The first commit authored within 2 minutes of the fork's creation (the
  change was written elsewhere).
- A thin record.

## Risk flags

| Level | Kind | Matches |
| --- | --- | --- |
| HIGH | `ci-or-automation` | `.github/**`, GitLab, Circle, Jenkins, Azure, Buildkite |
| HIGH | `agent-instructions` | `.claude/**`, `CLAUDE.md`, `AGENTS.md`, `GEMINI.md`, `.cursor*`, `.mcp.json`, `greptile.json`, `.coderabbit.yml` |
| HIGH | `build-or-packaging` | `pyproject.toml`, `setup.py`/`cfg`, `Makefile`, `Cargo.toml`, `build.rs`, `package.json`, Dockerfiles, compose files, `*.spec`, toolchain files |
| HIGH | `dependencies` | lockfiles, `requirements*.txt`, `go.mod`/`sum`, Gemfiles |
| HIGH | `tooling-that-runs-code` | `conftest.py`, `sitecustomize.py`, pre-commit, `.envrc`, `.gitmodules`, `.gitattributes`, `.npmrc`, `.vscode`, `.devcontainer` |
| HIGH | `binary` | compiled or archived files added |
| HIGH | `zero-width-characters`, `bidi-controls` | in added lines, title, body or commit messages (a single leading BOM is ignored) |
| HIGH | `base64-blob`, `pipe-to-shell`, `ci-secret-reference` | in added lines; the last two also in prose |
| HIGH | `prompt-injection` | text addressed to an AI reviewer. Prose gets the full pattern; code gets a narrower one, because an LLM project's code says "system prompt" legitimately. |
| ELEVATED | `scripts`, `project-policy` | shell scripts, `scripts/`, SECURITY, LICENSE, CODEOWNERS, GOVERNANCE |
| ELEVATED | `exec-or-network` | subprocess, eval/exec, pickle, ctypes, sockets, HTTP clients, `fetch("http…")`, `Command::new`, `unsafe {` |
| ELEVATED | `test-weakening`, `test-removed` | skip/xfail/ignore markers added; test files deleted |
| ELEVATED | `hidden-html-comment` | an HTML comment in the body that is neither in the PR template nor a short review-bot marker |
| ELEVATED | `size`, `no-diff` | more than 400 changed lines or 20 files; a file the API returned no diff for |

## Filling gaps by hand

In a claude.ai cloud session, `gh` reaches only `repos/{owner}/{repo}/...`, so
`collect` fills `pr` and leaves the rest as gaps. Fill those with the GitHub
MCP tools, as `ToolSearch` `select:` loads them. `L` is the login, `R` is
`owner/repo`, and `D30` and `D60` are the dates 30 and 60 days ago.

| Field | GitHub MCP query |
| --- | --- |
| `account.created_at` | `search_users` with `user:L created:<YYYY-MM-DD`. It returns 1 hit if the account is older. Binary-search the month and use its first day. |
| `account.followers`, `public_repos` | `search_users` with `user:L followers:>=N` or `repos:>=N`. Use the lower bound you can prove. |
| `account.profile_fields` | Leave `null`; search cannot see it. |
| `history.external_merged` | `search_pull_requests` with `author:L -user:L is:merged`, `perPage: 1`, `fields: ["number"]`, and read `total_count`. |
| `history.external_closed_unmerged` | The same with `is:closed is:unmerged`. |
| `history.external_open` | The same with `is:open`. |
| `history.first_external_pr_at` | `author:L -user:L` with `sort: created`, `order: asc`, `perPage: 1`. |
| `history.external_prs_30d`, `external_repos_30d` | `author:L -user:L created:>=D30` with `fields: ["created_at","repository_url"]` and `perPage: 100`. Count the distinct repos, and keep the times for `timestamps`. |
| `history.external_prs_60d` | `author:L -user:L created:>=D60`, `total_count`. |
| `history.spam_labelled` | Sum the totals of `author:L label:spam` and `author:L label:invalid`. |
| `history.fast_self_merges` | `author:L user:L is:merged` with `fields: ["created_at","closed_at"]`. Count PRs closed within 60 s of opening. |
| `repos.forks_30d`, `originals_30d` | `search_repositories` with `user:L fork:only created:>=D30` (or `fork:false`). |
| `repos.max_forks_24h` | The same query sorted by creation; find the busiest 24 hours. |
| `repos.achievement_named` | `search_repositories` with `user:L` and each achievement name `in:name`. |
| `here.merged`, `closed_unmerged`, `open` | `repo:R author:L` with `is:merged`, `is:closed is:unmerged`, `is:open`. |
| `timestamps` | The `created_at` values from the 30-day query, newest first. Twenty is enough. |
| `pr.*` | `collect` fills this whenever repository endpoints work. Otherwise use `pull_request_read` with `get`, `get_files`, `get_commits`, `get_reviews`, `get_comments` and `get_review_comments`. |

Set `gaps` to the fields you could not fill. The scorer treats a missing
field as unknown: it scores nothing and caps HIGH at MEDIUM.

## Calibration: October 2026

These are seven open first-time PRs to code-graph-rag. Each author was
investigated by hand, then scored (with the account sections filled as
above).

| Pattern | n | Tier | Hand verdict |
| --- | --- | --- | --- |
| Accounts 3–11 years old, quiet until September 2026, then 85–287 outside PRs in 30 days across 55–95 repos; spam labels, fork bursts, badge farming, or a 5-minute scheduler beat | 3 (4 PRs) | VERY LOW (0–10) | Agent farming |
| The same burst shape at lower volume (34 PRs in 30 days, most to one project), responsive to review | 1 | MEDIUM (60) | Agent farming; judge the code |
| No outside record; claimed the issue first and engaged | 1 | UNKNOWN (55) | Genuine |
| No outside record; silent for 11 days after a change request | 1 | MEDIUM (45) | Genuine, stalled |

None of the seven touched a sensitive path. Risk came out LOW or ELEVATED
(size, one platform `skipif`). The real hazards were logic bugs that only a
reader found:

- requirement `-r` includes followed outside the repository;
- a predictable temp file next to the file being edited;
- a delete query copied from before a fix.

That is why step 4i exists.

The first scoring run on these PRs also surfaced false positives, now fixed:

- a leading BOM in a PR body read as zero-width text;
- CodeRabbit's release-notes markers read as hidden HTML comments;
- a local function named `fetch` read as a network call.

## Blind spots

- **Patient attackers score well.** A long, real record, like xz-utils' "Jia
  Tan" over two years, earns HIGH. Only the risk axis and reading the change
  stand in the way.
- **Low-volume agents concentrated in one project** score MEDIUM. Checks 4c,
  4f and 4g catch them.
- **Merge rate counts trivial merges** (data-file edits, RPC-list cleanups)
  the same as real ones. See what got merged.
- **Search caps samples at 100.** Distinct-repo counts are therefore lower
  bounds, and large totals are approximate.
- **Spam labels exist only where maintainers apply them.**
- **The risk patterns are lists.** Text crafted to avoid them gets past them.
  Read check 4h as "read it", not "trust the flags".

## Sources

- Good Egg, trust scoring from merge history, with validation:
  https://github.com/2ndSetAI/good-egg (see `docs/methodology.md`)
- Vouch, an explicit trust list (the `VOUCHED.td` format):
  https://github.com/mitchellh/vouch
- anti-slop, rule-count PR screening with its default thresholds:
  https://github.com/peakoss/anti-slop
- croft's `scripts/vet_issue.py`, the hidden-content checks this reuses.
