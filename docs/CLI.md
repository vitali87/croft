# Running croft

The `croft` command line: opening a workspace, running croft on a remote host or in a dev
container, persistent sessions, and the subcommands you use from inside a pane. The
[README](../README.md#run) keeps the short version; `croft --help` lists every subcommand and
flag.

## Opening a workspace

```bash
croft                                # opens the current directory
croft ~/projects                     # opens a specific folder
croft ~/proj --open-file a.rs        # opens a folder with a file already open
croft ~/proj --open-file a.rs --zen  # ...focused on just the file (no sidebar/terminal)
```

## Remote hosts and dev containers

```bash
croft remote <host>          # launch croft over SSH on a Linux server (host from ~/.ssh/config)
croft devcontainer [folder]  # open a folder inside its dev container (.devcontainer/devcontainer.json)
```

`croft remote <host>` installs itself on the box on first connect with no manual prep, and a stock cloud image works as-is. See [LINUX.md](LINUX.md#remote-croft-remote-host) for how the cross-compile and host provisioning work.

`croft devcontainer` builds or pulls the image in `devcontainer.json`, starts the container with the folder mounted, runs `postCreateCommand` once, installs croft into it the same way, and runs it there. It supports `image`, `build`, `mounts`, `forwardPorts`, `postCreateCommand`, `workspaceFolder`, `containerEnv`, `remoteUser` and `runArgs`. The next run reuses the container, and an edited config (or `--rebuild`) replaces it.

## Persistent sessions

```bash
croft attach                    # open the current folder as a persistent session (survives closing the window)
croft attach ~/projects         # ...for a specific folder
croft attach --solo ~/projects  # join a shared folder in your own viewport (live co-editing)
croft ls                        # list running persistent sessions
```

See [COLLABORATION.md](COLLABORATION.md) for sessions, viewports, and the AI seat.

## From inside a pane

```bash
croft view report.pdf        # from any pane: open a file in the croft you are sitting in
cat data.csv | croft view -  # ...or pipe it in (staged to ~/.cache/croft, 0600, swept at the next launch once a day old)
```

## `croft://` links

```bash
croft open-link 'croft://attach?host=devbox&path=/srv/app'  # what a croft:// link runs (croft install-link-handler registers them)
croft open-link 'croft://decide?host=devbox&path=/srv/app&token=…&decision=allow'  # a notification's Approve: answers the agent's edit without attaching
```

## Themes, translations, and config sync

```bash
croft theme-import theme.json                   # use a VS Code colour theme in croft
croft theme-import dracula-theme.theme-dracula  # ...or fetch one from the marketplace
croft locale-template fr --write                # start or update a translation of croft's UI (see SETTINGS.md)
croft sync-config devbox                        # push your keybindings, snippets, triggers to a host now (never over its own edits)
```

## Guided tour

```bash
croft demo  # a guided tour in a throwaway sample project (--tour my.json runs your own steps)
```
