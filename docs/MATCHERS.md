# Problem matchers

croft turns command output into PROBLEMS entries with zero configuration:
every finished command in a terminal pane runs through built-in matchers
for rustc/cargo, tsc (both output shapes), gcc/clang and anything else
using the `file:line:col: severity: message` shape, Python tracebacks
(the deepest frame), and eslint's stylish format. Rerunning a command in
a pane replaces that pane's previous entries, so a clean rebuild clears
its errors.

`matchers.json` extends that to tools croft does not know, and to watch
tasks that never exit.

## matchers.json

Lives next to `triggers.json` in `~/.config/croft/` (palette:
**Preferences: Open Problem Matchers (JSON)**, reloaded on save). A
workspace may add its own at `<root>/.croft/matchers.json`; workspace
matchers are regexes applied to output — they never execute anything.
JSONC (comments, trailing commas) is fine.

```jsonc
[
  {
    "name": "mylint",
    "pattern": "^(?P<file>\\S+):(?P<line>\\d+):(?P<col>\\d+) (?P<severity>\\w+) (?P<message>.+)$",
    "severity_map": { "E": "error", "W": "warning" },
    "applies_to": "mylint*"
  }
]
```

Each entry:

- **`name`** — the source tag shown on the PROBLEMS row.
- **`pattern`** — a regex with NAMED capture groups: `(?P<file>…)` is
  required; `(?P<line>…)`, `(?P<col>…)`, `(?P<severity>…)`,
  `(?P<message>…)`, `(?P<code>…)` are optional. Line and column are the
  tool's 1-based numbers; severity defaults to error; a missing message
  falls back to the whole line; a captured code prefixes the message.
- **`patterns`** — instead of `pattern`: an array matched against
  consecutive lines, fields accumulating across the sequence (file from
  a header line, position and message from the rows under it). Entries
  are strings, or objects `{ "regex": "…", "loop": true }`; `loop` is
  allowed on the last entry only and re-applies it to each following
  line, emitting one diagnostic per match — the eslint shape.
- **`severity_map`** — extra severity words beyond the built-in
  error/warning/info/hint: `{ "E": "error" }`.
- **`applies_to`** — a glob (`*`, `?`) over the finished command line;
  the matcher only scans commands it claims. Omit to scan everything.
- **`background`** — `{ "begins": "…", "ends": "…" }` turns the matcher
  into a watch matcher (below).
- **`"enabled": false`** — keep an entry without running it.

Custom matchers run alongside the built-in table; a built-in row at the
same file/line/column as a custom row is dropped, so a matcher for a
gcc-shaped format never double-reports. Regexes that fail to compile are
dropped with a warning in **OUTPUT · Matchers** at load — a broken entry
never blocks startup or the stream path.

## Watch tasks (background matchers)

`tsc --watch`, `cargo watch`, vite: long-running watchers never hit the
finished-command boundary, so batch scanning never sees them. A matcher
with a `background` block runs on the live stream instead: when a
pane's output matches `begins`, croft starts collecting; on `ends` the
collected window is scanned and published to PROBLEMS, replacing the
pane's previous batch. Every recompile cycle republishes — and a clean
cycle publishes an empty batch, which is what clears the errors you just
fixed. The watcher's own exit does not re-scan its scrollback, so old
cycles never resurrect.

```jsonc
[
  {
    "name": "watchful",
    "pattern": "^(?P<file>\\S+): (?P<message>.+)$",
    "background": { "begins": "^BUILD START$", "ends": "^BUILD END$" }
  }
]
```

A background matcher with no `pattern` scans its window with the
built-in table — enough for any watcher whose per-error format is
already covered.

## tasks.json `problemMatcher`

A `.vscode/tasks.json` task's `problemMatcher` is honoured when the task
runs (Tasks: Run Task / Run Build Task):

- The well-known names **`$tsc`**, **`$tsc-watch`**, **`$rustc`**,
  **`$eslint-stylish`**, **`$gcc`** map onto the built-in table,
  narrowed to that tool's rows. `$tsc-watch` additionally carries tsc's
  watch-cycle delimiters, so a `tsc --watch` task repopulates PROBLEMS
  on every recompile without exiting.
- An inline matcher object translates into the same machinery:
  `pattern` with VS Code's numeric group indices (`"file": 1`, `"line":
  2`, `"column": 3`, plus `severity`/`message`/`code`/`loop`), an
  optional `base`, and `background.beginsPattern`/`endsPattern` (string
  or `{ "regexp": … }`).
- An unknown `$name` degrades to the built-in first-match-wins scan
  rather than erroring.

While a task pane has an explicit matcher, that matcher is exclusive
for it (VS Code's model: declaring one replaces the defaults). Rerunning
the task replaces its previous diagnostics; running a matcher-less task
in the same pane clears the assignment.

**Problems: Clear Build Diagnostics** wipes every build-sourced entry;
LSP diagnostics are untouched either way.
