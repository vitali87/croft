# Extensions: a small core, features on demand

This is the plan for tenet 8, **one binary for the core; extensions arrive in one
click**. The core binary carries only what every session needs. Everything else
becomes an extension that croft downloads, verifies and installs when someone
adds it in the Extensions view.

There are three aims, and none of them may cost correctness:

1. **A smaller download.** Users who never open a SQLite file or a CodeQL
   database should not carry the code for them.
2. **Faster builds.** A one-line change should not wait for code it does not
   touch, and a release should not take a quarter of an hour.
3. **A faster croft.** Less code in the core means less to load, and an
   extension that is not in use costs nothing.

Each step below is measured against those three before it lands, and a step
lands only if the existing tests pass unchanged.

## Where we start

Measured on 2026-10-31 on a 4-core Linux machine at v0.2.99:

| Part | Time | Binary share (63.5 MB) |
|---|---|---|
| Release build, from scratch | 14 min | |
| ... croft's own code, one crate | 11 min 52 s | most of it |
| ... every dependency, mostly in parallel | about 2 min | |
| ... the 25 tree-sitter grammars | 30 s | about 24 MB (compiled archives) |
| ... bundled SQLite | 49 s | about 3 MB |
| Debug build, from scratch | 3 min | |
| Debug rebuild after a one-file change | 14 s | |

Two facts follow from the table:

* **Build time is croft's own code.** About 271,000 non-test lines compile as a
  single crate, and the release profile (`codegen-units = 1`) optimises that
  crate on one core. Moving dependencies out barely changes the build.
* **Binary size is mostly grammars.** The grammars are over a third of the
  binary, and most files a user opens need only a handful of them.

The code is also tightly knotted. Of the 271,000 lines, only about 25,000 sit in
modules that reach nothing else in the crate. The rest form one cycle through
`app`, `widgets`, `lsp` and `prefs`. Splitting the crate therefore starts with
untangling, not with moving files.

The features that are natural extensions (CodeQL, SARIF, the file viewers, the
AI seats and agent hooks, VS Code import, voice and a few others) add up to about
37,000 lines in their own modules, and more once their side bars and app wiring
are counted. Moving them out shrinks the core by a sixth to a fifth. Build speed needs both moves: extensions out
**and** the remaining core split into crates that compile in parallel.

## What an extension is today

An extension is an `extension.toml` manifest, data only (see
`src/lsp/manifest.rs`). Some manifests point at a program croft runs: a
language server, a debug adapter, an MCP server, a terminal viewer such as
csvlens. Others switch on native code that is already in the binary. CodeQL is
the clearest case: adding it from the signed index writes a manifest, and
`App::is_extension_enabled("codeql")` then shows a side bar that was compiled in
all along. The bundled ones (`assets/extensions/`) are baked in and toggled.

## What an extension becomes

**An extension is a manifest plus, optionally, a program built for each
platform.** The program runs as its own process, started the first time it is
needed, and talks to croft over stdin and stdout with JSON messages, the same
shape croft already speaks to language servers and MCP servers. croft draws
everything itself, with building blocks it already has.

Why a separate process, rather than loading code into croft:

* **Linux releases are static musl binaries, and a static binary cannot load a
  shared library.** Native plugins would work on macOS and Android and fail on
  Linux, which tenet 4 forbids.
* **Rust has no stable ABI.** A plugin library built against a different croft
  version is undefined behaviour, not an error message.
* **An embedded WebAssembly runtime would add megabytes and minutes of build to
  the core,** the opposite of the goal.
* **A crash or a hang stays in the extension.** croft reports it and carries on.

### Delivery

* **The signed index gains a `payloads` table** for each entry: one file per
  target triple, with its URL, size and sha256. The ed25519 signature
  (`src/mcp/registry_index.rs`) covers it, so the host serving the files stays
  untrusted, as the manifests are today.
* **Adding an extension** downloads the payload for the running target, checks
  its sha256 against the signed index, unpacks it under
  `~/.croft/extensions/<id>/<version>/` (beside the managed language servers in
  `~/.croft/servers/`), and only then writes the manifest into the user
  extensions folder (`~/.config/croft/extensions/<id>/`). An interrupted or
  failed download leaves nothing installed. The download code
  already exists for language servers (`Provision::Binary` in
  `src/lsp/install.rs`) and is reused.
* **Remote parity.** `croft remote` installs the payload for the server's own
  target through the same path, so an extension works the same over SSH.
* **Offline machines** install from a bundle: `croft extension install
  <bundle>`. A fresh offline machine has no cached index, so the bundle carries
  the payload together with `index.json` and its signature. croft verifies the
  signature against the baked key and the payload against the signed sha256,
  exactly as an online install does, and refuses a bundle that fails either
  check.
* **Versions.** The manifest's `api_version` names the protocol version the
  program speaks. The index offers only payloads this croft can talk to, as it
  already hides manifests it cannot read.
* **The manifest names the exact payload it runs.** `api_version` is the
  protocol, not the payload's identity, so the manifest also carries the
  extension's `version` (the same version the signed index entry lists) and the
  program's path inside the payload. Install writes the manifest only after
  that version is unpacked and the named program exists in it. Launch runs only
  `~/.croft/extensions/<id>/<version>/<path>` for the version the manifest
  names, and reports a missing or incomplete payload instead of starting
  anything else.
* **Upgrades** unpack the new version beside the old one and replace the
  manifest last, so a failed upgrade leaves the old version installed and
  working. The old version's folder is removed once nothing runs from it.
* **Start-up.** No extension starts before the first frame (tenet 1). A program
  starts when its feature is first used: opening a SQLite file, clicking the
  CodeQL icon.

### Where extensions are built

Extension programs live in this repository as workspace members marked
`publish = false`, so they share CI, tests and review with the core. The release
workflow builds each one for every target and attaches them to the GitHub
release. The index build in
[croft-extensions](https://codeberg.org/vitali87/croft-extensions) reads their
URLs and hashes and signs them with the rest of the index.

### The protocol, in outline

The full message set is specified in phase 3. Its shape:

* **Handshake:** croft sends `initialize` with its protocol version, and the
  program answers with what it provides.
* **From croft:** open a file, run a command, cancel a request, shut down.
* **From the extension:** a view to draw, an update to it, diagnostics, a status
  line, progress.
* **Views croft draws natively:** a table (the sheet grid that already serves
  CSV, spreadsheets and SQLite), a tree, a list, styled text and Markdown,
  diagnostics. An extension describes what to show; croft owns how it looks, so
  themes, shortcuts and accessibility stay consistent (tenets 5 and 6).

## Grammars

Grammars move out last among the size wins, because highlighting sits on the
typing path. The plan:

* **Keep a small core set built in,** chosen by how often each language's files
  are opened, so most sessions never fetch one.
* **Serve the rest from a syntax extension:** a process that holds the parse
  tree, takes edits as they happen, and returns highlight spans for the visible
  lines, plus the outline and folding ranges. It uses the same grammars and the
  same queries, so the colours do not change.
* **Measure keystroke-to-paint latency before and after.** The move lands only
  if typing in an extension-highlighted file stays inside the frame budget.

Largest grammar sources today: C# (29 MB of C), SQL and C++ (17 MB each), Ruby
(15 MB), Bash (10 MB), TypeScript (9 MB), Rust (7 MB).

## Splitting the core

The core becomes a Cargo workspace of crates by area, compiled in parallel.
Thin LTO stays on, so code is still inlined across crates and croft runs no
slower.

`cargo install croft-software` builds from crates.io, so every library crate the
binary depends on has to be published there too. `cargo publish --workspace`
publishes them in order, but each new crate name needs one first publish by the
owner before crates.io Trusted Publishing can be set up for it. Extension
programs are not dependencies of the binary and are never published to
crates.io.

## Phases

Each phase is a series of small pull requests (CONTRIBUTING.md asks for about
400 changed lines at most). Each reports the binary size, release build time,
time to first frame and keystroke latency before and after.

1. **Untangle.** Break the cycles that keep modules from standing alone, with
   no change in behaviour: a module that reaches into `app` or `widgets` for one
   helper gets that helper moved down to where both can reach it.
2. **Workspace.** Turn the repository into a workspace and move the first
   library crates out, leaves first. Needs the crates.io setup above.
3. **Extension host.** The protocol, payload download and verification, the
   index's `payloads` table (a change in croft-extensions too), install from a
   file, remote install, and Extensions view states for downloading and failed
   installs.
4. **File viewers out:** SQLite, spreadsheets (calamine and umya-spreadsheet),
   Word and OpenDocument, archives, log colouring, notebooks.
5. **Grammars out,** behind the latency check above.
6. **Features with their own screens out:** CodeQL and SARIF, the AI seats (the
   MCP collab agent and the `croft pair` navigator) and agent edit approvals,
   VS Code import, voice, `croft web`, and the rest that most sessions never
   open. Shared sessions and `--solo` viewports are not on this list: they are
   how `croft attach` works (see COLLABORATION.md), so they stay with the
   session host in the core, and the AI seats keep joining them as guests.

## What stays in the core

The editor, the terminal, the Explorer, Search, Source Control, the LSP and DAP
clients, tasks, the Testing view's runner host, settings and themes,
`croft remote`, `croft attach` with its shared sessions and `--solo` viewports,
and the extension host itself.

## Open questions

* **The core grammar set.** Which languages stay built in, decided from data
  rather than taste.
* **Where payloads are hosted.** GitHub release assets or croft's own domain;
  either works, since the signed hash, not the host, is what is trusted.
* **Crates.io.** Publishing a library crate per area, or a smaller number of
  larger ones to keep the one-time setup short.
