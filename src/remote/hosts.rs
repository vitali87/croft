//! The SSH hosts croft knows: the targets in ~/.ssh/config and the state of each entry,
//! and the host a running ssh is connected to, read from its argv with ssh's own flag
//! grammar.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteTarget {
    pub alias: String,
    pub host_name: Option<String>,
    pub user: Option<String>,
}

impl RemoteTarget {
    pub fn detail(&self) -> String {
        match (&self.user, &self.host_name) {
            (Some(user), Some(host)) => format!("{user}@{host}"),
            (Some(user), None) => format!("{user}@{}", self.alias),
            (None, Some(host)) if host != &self.alias => host.clone(),
            _ => String::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SshConfigState {
    entries: Vec<SshConfigEntryState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SshConfigEntryState {
    path: PathBuf,
    modified: Option<std::time::SystemTime>,
    len: Option<u64>,
}

pub fn ssh_config_state() -> SshConfigState {
    SshConfigState {
        entries: ssh_config_paths()
            .into_iter()
            .map(|path| {
                let meta = std::fs::metadata(&path).ok();
                SshConfigEntryState {
                    path,
                    modified: meta.as_ref().and_then(|m| m.modified().ok()),
                    len: meta.map(|m| m.len()),
                }
            })
            .collect(),
    }
}

#[derive(Default)]
pub(super) struct HostBlock {
    aliases: Vec<String>,
    host_name: Option<String>,
    user: Option<String>,
}

/// Programs whose command line this understands.
///
/// **`ssh` only, and that is a correctness limit rather than an oversight.**
/// `mosh` and `et` reach a box the same way and were in this list, but they
/// take LONG options and `ssh` does not — and the parse below is built on
/// ssh's single-letter grammar. `mosh --port 60000 box` yields `60000`,
/// because `strip_prefix('-')` leaves `-port`, the scan finds `p` mid-word,
/// and the attached-value branch consumes nothing. That is a wrong host, not
/// a refusal, and a wrong host here re-roots the workspace onto the wrong
/// machine. Parsing them properly needs a per-program table of which long
/// options take a separate word; until that exists, refusing is the only
/// honest answer.
///
/// `sshfs` is absent for a different reason: it mounts a filesystem rather
/// than giving you a shell, so re-rooting onto it answers a different
/// question. `ssh-agent` and `ssh-keygen` share a prefix with `ssh` and
/// nothing else, which is why the match is on the whole name.
pub(super) const SSH_PROGRAMS: [&str; 1] = ["ssh"];

/// `ssh` flags that take a SEPARATE argument, per ssh(1).
///
/// This list is what makes the parse work: without it, `ssh -p 2222 box`
/// yields `2222` as the destination, because `2222` is the first word that
/// does not begin with `-`. The boolean flags need no listing — anything
/// starting with `-` that is not here is simply skipped, so a SHORT flag
/// added to ssh in future degrades to "skipped" rather than to a wrong host.
/// That guarantee does NOT extend to long options — `--port` scans as `-port`
/// and finds `p` mid-word — which is why [`SSH_PROGRAMS`] admits only `ssh`,
/// the one program in this family that has none.
pub(super) const SSH_FLAGS_WITH_ARG: [char; 22] = [
    'B', 'b', 'c', 'D', 'E', 'e', 'F', 'I', 'i', 'J', 'L', 'l', 'm', 'O', 'o', 'P', 'p', 'Q', 'R',
    'S', 'W', 'w',
];

/// The destination an `ssh`-family command line names, or `None` when the
/// line is not one of those programs or names no host yet (#364).
///
/// `cmd` is the process's argv. The rule is ssh's own: skip flags, skip the
/// argument of any flag that takes one, and the first remaining word is the
/// destination — everything after it is the remote command.
///
/// Deliberately pure and argv-shaped rather than reading `/proc`: the whole
/// difficulty is the flag grammar, and a function that takes argv can be
/// swept against real invocations without a live process.
pub fn ssh_destination(cmd: &[&str]) -> Option<String> {
    // `rsplit` always yields at least one item, so there is no empty case.
    let program = cmd.first()?.rsplit('/').next()?;
    if !SSH_PROGRAMS.contains(&program) {
        return None;
    }
    let takes_arg = |c: char| SSH_FLAGS_WITH_ARG.contains(&c);
    let mut rest = cmd[1..].iter();
    while let Some(word) = rest.next() {
        // `--` ends option parsing: the next word is the destination even if
        // it begins with a dash. Without this, a host named like a flag is
        // skipped and the parse runs off the end.
        if *word == "--" {
            return rest
                .next()
                .and_then(|w| (!w.is_empty()).then(|| (*w).to_string()));
        }
        let Some(flags) = word.strip_prefix('-') else {
            // The first non-flag word is the destination. An empty string is
            // not a host, and neither is a bare `-`.
            return (!word.is_empty()).then(|| (*word).to_string());
        };
        if flags.is_empty() {
            continue;
        }
        // Bundled booleans (`-4qt`) take nothing. A flag that takes an
        // argument consumes the REST OF THIS WORD if there is any (`-p2222`),
        // and otherwise the next word.
        for (i, c) in flags.char_indices() {
            if takes_arg(c) {
                if i + c.len_utf8() == flags.len() {
                    rest.next();
                }
                break;
            }
        }
    }
    None
}

pub fn discover_ssh_targets() -> Vec<RemoteTarget> {
    ssh_config_paths()
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .flat_map(|content| parse_ssh_config(&content))
        .fold(
            (BTreeSet::new(), Vec::new()),
            |(mut seen, mut targets), target| {
                if seen.insert(target.alias.clone()) {
                    targets.push(target);
                }
                (seen, targets)
            },
        )
        .1
}

pub(super) fn ssh_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".ssh/config"));
    }
    paths
}

pub fn primary_ssh_config_path() -> Option<PathBuf> {
    ssh_config_paths().into_iter().next()
}

pub fn parse_ssh_config(input: &str) -> Vec<RemoteTarget> {
    let mut targets = Vec::new();
    let mut block = HostBlock::default();

    for raw in input.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else {
            continue;
        };
        let key = key.to_ascii_lowercase();
        if key == "host" {
            flush_block(&mut targets, &mut block);
            block.aliases = parts
                .filter(|alias| is_explicit_host_alias(alias))
                .map(ToString::to_string)
                .collect();
            continue;
        }
        if block.aliases.is_empty() {
            continue;
        }
        match key.as_str() {
            "hostname" => block.host_name = parts.next().map(ToString::to_string),
            "user" => block.user = parts.next().map(ToString::to_string),
            _ => {}
        }
    }
    flush_block(&mut targets, &mut block);
    targets
}

pub(super) fn flush_block(targets: &mut Vec<RemoteTarget>, block: &mut HostBlock) {
    for alias in block.aliases.drain(..) {
        targets.push(RemoteTarget {
            alias,
            host_name: block.host_name.clone(),
            user: block.user.clone(),
        });
    }
    block.host_name = None;
    block.user = None;
}

pub(super) fn strip_comment(line: &str) -> &str {
    line.split_once('#').map(|(head, _)| head).unwrap_or(line)
}

pub(super) fn is_explicit_host_alias(alias: &str) -> bool {
    !alias.starts_with('!') && !alias.contains('*') && !alias.contains('?')
}
