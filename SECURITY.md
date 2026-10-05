# Security Policy

croft is a terminal IDE that runs as you, on your machine or over SSH on a
machine you already have an account on. This document says which versions
get security fixes, how to report a vulnerability privately, what counts as
one, and how a fix reaches you.

## Supported versions

croft ships as a rolling release. Every merge to `main` that changes what
ships is tagged and published to GitHub Releases, crates.io and the Homebrew
tap, so there is no maintained branch behind the latest tag. Security fixes
land in a new release, never as a patch to an old one.

| Version | Supported |
| --- | --- |
| The [latest release](https://github.com/vitali87/croft/releases/latest) | :white_check_mark: |
| Any earlier tag | :x: |
| Builds from `main` | :white_check_mark: on a best-effort basis: reports are welcome, but a fix ships as the next tag, not a re-pointed one |

Before reporting, please reproduce on the latest release or on `main`. croft
tells you when a newer release exists (the update watcher in the status bar,
or **F9**), and each of `brew upgrade croft`, `cargo binstall croft-software`
and `cargo install croft-software --locked` brings you to it.

## Reporting a vulnerability

**Please do not open a public issue, discussion or pull request for a
security problem.** A public report gives attackers a head start on users who
have not updated yet.

Report privately through GitHub's private vulnerability reporting:

**https://github.com/vitali87/croft/security/advisories/new**

This opens a draft security advisory that only you and the maintainers can
see. It supports a private fork for proposing a fix, and it becomes the
published advisory (with a CVE, if warranted) once a fix is out.

If that form is unavailable to you, open a regular issue that says only
"security report, please open a private channel" with no details, and a
maintainer will reach out to set one up.

### What to include

The more of this you can give, the faster the triage:

- The croft version (`croft --version`) or commit, and how it was installed
  (Homebrew, `cargo binstall`, `cargo install`, source build).
- Operating system and terminal, and whether croft was running locally, over
  `croft remote`, inside `croft devcontainer`, as a `croft attach` session
  host, or with `croft web`.
- Step-by-step reproduction: the smallest configuration, file, link, server
  response or byte sequence that triggers it.
- What an attacker gains: what boundary is crossed and from where (another
  local account, a web page, a repository you opened, a language server, a
  remote host).
- Any proof-of-concept, crash log or backtrace. Please keep proofs of concept
  harmless (no real exfiltration, no persistence on machines you do not own).

You may report anonymously. If you want credit in the advisory and release
notes, say how you would like to be named.

### What to expect

The maintainers aim for the following. croft is a small project, so these are
targets rather than a guarantee, but you will always hear back.

| Step | Target |
| --- | --- |
| Acknowledgement that the report was received | 3 business days |
| Initial assessment: confirmed or not, and a provisional severity | 10 days |
| Fix released for a confirmed critical or high severity issue | 30 days |
| Fix released for a confirmed medium or low severity issue | 90 days |

Severity follows [CVSS v3.1](https://www.first.org/cvss/v3.1/specification-document),
adjusted for croft's trust model below (a finding that needs the user's own
account is not high severity, however dramatic it looks). You will be kept
informed as the fix progresses and asked to confirm it where you can.

### Coordinated disclosure

We ask for a 90-day embargo from the date of the report, or until a fixed
release is published, whichever comes first. If a fix will take longer, we
will tell you why and agree a new date rather than let the deadline pass in
silence. If the issue is being exploited in the wild, we will publish sooner.

When the fix ships, the advisory is published on the repository's
[Security Advisories](https://github.com/vitali87/croft/security/advisories)
page and the release notes name the fix. Reporters are credited unless they
ask not to be.

## Scope

### Trust model

croft has no accounts of its own and no privileged daemon. The security
boundary is the operating system account it runs as:

- **Local sessions** (`croft attach`, agent approvals, view IPC) talk over
  Unix domain sockets created at mode 0600 in your own runtime directory.
  Possession of the account is the trust boundary, by design.
- **Remote sessions** (`croft remote <host>`) ride your existing SSH
  connection and install croft onto the host into your own account. croft
  adds no listener and no authentication scheme of its own on the host.
- **`croft web`** is the one network listener. It binds loopback by default,
  authenticates every connection with a 128-bit random token that is never
  placed in a URL, checks `Origin` against `Host` and refuses non-loopback
  `Host` names on a loopback listener, and enforces write control server-side.
  Its threat model is written up in [docs/WEB.md](docs/WEB.md#threat-model).
- **`croft://` links** may be handed to the OS by any web page, so a link can
  only do what you could already do by typing the command: the host must be
  an alias in your own `~/.ssh/config`, arguments may not begin with `-`, and
  a `decide` link needs the one-time token from the notification it answers.
- **Language servers, debug adapters, MCP servers, kernels and themes** are
  third-party code croft runs on your behalf. croft treats their protocol
  output as untrusted input, but it does not sandbox them: installing one is
  the same decision as installing any other tool.

### In scope

Anything that crosses one of those boundaries without the user's consent.
Examples of what we want to hear about:

- **Code execution from data.** Opening a repository, file, theme, snippet,
  task definition, `.editorconfig`, `devcontainer.json`, `.http` file or
  notebook makes croft run something the user did not ask for.
- **`croft web`**: connecting without the token, a bypass of the `Origin` or
  `Host` checks, DNS rebinding, token disclosure through logs or URLs, or a
  read-only participant obtaining write control.
- **Session sockets**: another account on the machine reading, joining or
  hijacking a `croft attach` session, or a race that leaves a socket or its
  directory more permissive than 0600.
- **`croft://` links**: a crafted link that reaches a host not in the user's
  SSH config, injects a flag or shell fragment, or answers an agent approval
  without its token.
- **Agent approvals**: an agent's Edit, Write or MultiEdit landing on disk
  without the approval prompt, or the prompt showing a different diff from
  what is applied.
- **Supply chain and updates**: the F9 updater, `croft remote` provisioning
  or `croft devcontainer` installing a binary that does not match the signed
  `SHA256SUMS`; a way to make cosign verification pass on a tampered archive;
  a release workflow flaw that would let an unsigned artifact ship.
- **Secrets**: an API token, SSH key or the web token written to a log,
  a synced config (`croft sync-config`), `pair.json`, the session recording,
  or a capture.
- **Untrusted protocol input**: a malicious LSP, DAP or MCP server response,
  or terminal output from a running program, that reads or writes files
  outside the workspace, escapes the pane, or corrupts memory in croft.
- **Memory safety** in croft's own `unsafe` code or in its parsers for PDF,
  images, DOCX, spreadsheets, SQLite, archives, asciicast and SARIF files,
  reachable by opening a file.
- **Path traversal** when extracting archives, installing a marketplace
  theme or extension, or provisioning a remote host.

### Out of scope

These are not vulnerabilities in croft. Please report them upstream or as an
ordinary issue instead:

- Anything that requires the user's own account, shell or SSH key. Whoever
  owns the account already owns the session; croft does not defend against
  the user.
- A vulnerability inside a language server, debug adapter, MCP server,
  kernel, extension or theme itself, rather than in how croft handles its
  output. Report it to that project. (A croft-side bug in *consuming* it is
  in scope.)
- Running `croft web` with a routable `--bind` and no `--tls`. croft serves
  it and warns that anyone on the path sees the token and the screen; that
  is documented behaviour, not a bug.
- Denial of service by flooding the `croft web` listener or a session socket,
  or by opening a pathologically large file. croft deliberately does not rate
  limit; a crash or hang with no security consequence is a normal bug report.
- A vulnerable dependency reported by an automated scanner without a
  reachable path through croft. We do still want to know, as a normal issue
  or pull request bumping it, and we will treat it as a vulnerability if you
  show it is reachable.
- Findings against the terminal emulator, shell, SSH client or operating
  system croft runs in.
- Social engineering, physical access, and reports generated by tooling
  with no reproduction.

If you are not sure which side of the line something falls on, report it
privately anyway. A false alarm costs a few minutes; a public zero-day costs
users.

## Safe harbour

We consider security research done in good faith under this policy to be
authorised. We will not pursue or support legal action against you for it,
and we will work with you to understand and resolve the issue quickly. Good
faith means you:

- test only against installations and accounts you own or have permission
  to test;
- do not access, modify or exfiltrate data belonging to anyone else, and
  stop and report as soon as you can demonstrate the issue;
- do not degrade the service of others (no flooding, no persistence);
- give us the embargo above before disclosing publicly.

## How security fixes reach you

A fix ships as a normal release, and the advisory is published alongside it.
To find out:

- Watch the repository's **Releases** and **Security Advisories**, or set a
  GitHub watch on "Security alerts".
- Inside croft, the update watcher shows a newer release in the status bar,
  and **F9** installs it (for a Homebrew install it defers to `brew`).
- Release notes for every tag are in the release itself.

## Verifying what you install

Every release carries `SHA256SUMS` covering all four prebuilt archives, and
that file is signed keylessly with [Sigstore](https://www.sigstore.dev/) by
the release workflow, so the signature proves it came from this repository's
`release.yml` run for that tag. Check the signature, then the sum of the one
archive you fetched (replace the tag with the one you downloaded):

```bash
cosign verify-blob \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity "https://github.com/vitali87/croft/.github/workflows/release.yml@refs/tags/v0.1.963" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  SHA256SUMS

grep "croft-aarch64-apple-darwin.tar.gz" SHA256SUMS | sha256sum -c -
# macOS: grep "..." SHA256SUMS | shasum -a 256 -c -
```

The Homebrew formula is rendered from that same signed `SHA256SUMS`, so
`brew` verifies the identical sums. The F9 updater checks a download against
`SHA256SUMS` and, when `cosign` is installed, verifies the signature first;
it reports plainly when it could not. Building from source with
`cargo install croft-software --locked` (or `--locked` on a checkout) builds
exactly the dependency set in the committed `Cargo.lock`.

## Supply-chain practices

What the project does to keep what ships trustworthy:

- **Pinned toolchain.** `rust-toolchain.toml` fixes the exact Rust version so
  every machine compiles with the same compiler.
- **Locked dependencies.** `Cargo.lock` is committed, releases build with
  `--locked`, and a dependency change needs release notes that CI enforces
  and a new version, assigned after merge.
- **Pinned GitHub Actions.** Third-party actions are pinned to a commit SHA,
  not a movable tag, and Dependabot keeps those pins current weekly.
- **Keyless signing.** Releases are signed with Sigstore from the workflow's
  own OIDC identity; there is no long-lived signing key to steal.
- **Pinned helpers.** Language servers, debug adapters and MCP servers are
  installed at exact pinned versions, with checksums where the ecosystem
  provides them, and are spoken to over stdio only, never a network listener.
  An MCP server's approved tool definition is re-verified every time it is
  spawned.
- **Static analysis.** Clippy runs in CI, and SonarCloud scans the tree.

Suggestions that strengthen any of this are welcome as ordinary pull
requests; see [CONTRIBUTING.md](CONTRIBUTING.md).
