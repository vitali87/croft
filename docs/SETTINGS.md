# Settings

croft's settings are JSON files merged as a chain of layers. Later layers win,
key by key:

1. **Built-in defaults** — what a fresh install behaves like.
2. **User** — `~/.config/croft/config.json` (or `$XDG_CONFIG_HOME/croft/`).
   The file every toggle and the theme picker write to.
3. **User, machine-local** — `~/.config/croft/config.local.json`. Overrides
   for this machine only; keep `config.json` in your dotfiles repo and put
   the per-box exceptions here.
4. **`.vscode/settings.json`** — a small mapped subset (see below), so a repo
   configured for VS Code behaves sensibly without duplication.
5. **Workspace** — `<root>/.croft/config.json`, committed. Team settings
   every collaborator gets on opening the repo.
6. **Workspace, local** — `<root>/.croft/config.local.json`, never committed.
   croft writes `config.local.json` into `.croft/.gitignore` before it ever
   creates the file.

Open any layer from **Preferences: Open Settings** (the hub lists all four
editable layers) or the palette commands "Preferences: Open Settings (JSON)"
and "Preferences: Open Workspace Settings (JSON) / — Local (JSON)". Saving a
layer file applies live: theme, editor toggles, save behavior, and host
accents re-merge on the spot; the few startup-read settings (layout,
terminal scrollback) still need a relaunch. The hub shows where a value came
from (`· workspace`, `· user-local`, …) whenever a layer other than your own
user config decided it.

In a multi-root workspace the workspace layers come from the **primary**
root. Re-rooting re-merges against the new primary.

## Merge rules

- **Objects deep-merge.** `{"explorer_views": {"timeline": false}}` in one
  layer and `{"explorer_views": {"outline": false}}` in a later one yields
  both; untouched keys keep their earlier values.
- **Arrays and scalars replace.** A later `host_accents` list replaces the
  whole earlier list; lists never concatenate, so a layer can narrow as well
  as extend.
- **Comments are fine.** All layer files tolerate `//` and `/* */` comments
  and trailing commas (JSONC), like `.vscode/tasks.json`.
- **A mistyped value only loses itself.** `"terminal_scrollback": "lots"`
  drops that one key with a warning; every other setting still applies.

Warnings — refused keys, parse failures, `extends` cycles — go to the
**OUTPUT · Settings** channel, and the status bar counts them on reload.

## `extends`

Any croft layer may compose other JSON files:

```jsonc
{
  "extends": ["~/dotfiles/croft-base.json", "./theme-overrides.json"],
  "format_on_save": true
}
```

Bases merge first, in listed order; the file's own keys win over them.
Paths resolve relative to the extending file, `~/` expands to your home
directory, and a cycle stops with a warning instead of hanging.

## Platform scopes

Top-level `"macos"`, `"linux"`, and `"android"` blocks merge over the layer's
flat keys on the matching platform only:

```jsonc
{
  "theme": "black",
  "linux": { "theme": "nord" }
}
```

Because a remote croft is a real Linux build, the `"linux"` block is also
what applies over `croft remote`.

## What workspace layers may set

Workspace files (4–6 above) are repo-controlled input: cloning a repo must
never change what croft trusts or executes. They are limited to an explicit
allowlist — appearance and editor/terminal behavior:

`theme`, `format_on_save`, `format_on_type`, `auto_save`, `auto_save_on_focus_change`,
`render_whitespace`, `disable_inline_blame`, `disable_auto_close_pairs`,
`disable_inline_values`, `disable_bracket_colors`, `disable_indent_guides`,
`disable_inlay_hints`, `copy_on_select`, `disable_secret_redaction`, `explorer_views`.

Everything else — `disabled_extensions`, `mcp_consented`,
`mcp_tool_fingerprints`, `host_accents`, `notifications`, and any future key not explicitly
allowlisted — is ignored from workspace layers with a visible warning.
Extending the allowlist is a deliberate review decision, not a default.

## The `.vscode/settings.json` subset

When present, croft maps the keys below (and nothing more: broad VS Code
settings compatibility is a non-goal). This is the same table
`croft import-vscode` uses for your user-level `settings.json`, filtered to
the keys a workspace layer is allowed to set, so a given VS Code setting
means the same thing whichever path reads it.

| VS Code | croft |
|---------|-------|
| `editor.formatOnSave` | `format_on_save` |
| `editor.formatOnType` | `format_on_type` |
| `files.autoSave: "afterDelay"` | `auto_save: true` |
| `files.autoSave: "onFocusChange"` / `"onWindowChange"` | `auto_save_on_focus_change: true` |
| `files.autoSave: "off"` | both auto-save modes off |
| `editor.renderWhitespace` | `render_whitespace` (`none` / `selection` / `all`; VS Code's `boundary` and `trailing` have no croft mode and map to `none`) |
| `editor.bracketPairColorization.enabled` | `disable_bracket_colors` (negated) |
| `editor.guides.indentation` | `disable_indent_guides` (negated) |
| `editor.inlayHints.enabled` | `disable_inlay_hints` (negated) |
| `editor.autoClosingBrackets: "never"` | `disable_auto_close_pairs` |
| `terminal.integrated.copyOnSelection` | `copy_on_select` |

Keys croft honours only as user config (a terminal scrollback, say) are
mapped by `croft import-vscode` but ignored here, since a workspace may not
set them.

The mapped values sit **below** `.croft/config.json` in the chain, so a
croft-native workspace file always wins over the VS Code one.

## Notification sinks

A `notifications` list forwards events croft already notices to somewhere
you will see them away from the terminal (#358). Each entry names a sink
and, optionally, which events it takes:

```json
{
  "notifications": [
    { "kind": "ntfy", "topic": "my-croft", "events": ["command_finished", "tests_failed"], "min_duration_secs": 30 },
    { "kind": "webhook", "url": "https://hooks.example.org/croft", "headers": { "authorization": "Bearer …" } },
    { "kind": "termux" },
    { "kind": "command", "argv": ["/usr/bin/osascript", "-e", "display notification (system attribute \"CROFT_BODY\")"] }
  ]
}
```

Events are `command_finished` (a command in a pane you are not focused on,
lasting at least `min_duration_secs`, default 10), `tests_failed` (once per
red Test Explorer run), `osc9` (a terminal's own notification), and
`agent_waiting` (reserved for #344). An empty `events` takes all of them.
`ntfy` posts to `server` (default `https://ntfy.sh`) under `topic`; both `ntfy` and `webhook` endpoints must be `https`, or `http` only to localhost — a plain-http endpoint across a network is refused and named in the channel;
`webhook` posts JSON `{event, title, body, workspace, host, link}` with your
`headers`; `termux` runs `termux-notification`; `command` runs `argv` with
the notification in `CROFT_TITLE`, `CROFT_BODY`, `CROFT_LINK`, and
`CROFT_EVENT`. Delivery is off the render path on one worker with a bounded
queue and one retry for transient failures; what finally fails appears in
the **Notifications** OUTPUT channel, naming only a URL's host. The key is
user-config only, and a webhook's headers belong in `config.local.json`.
