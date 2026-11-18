//! The ssh-pane offer: which host and folder a pane's ssh is in, whether to offer to
//! re-root there, and the hosts whose install failed, remembered for a week.

use super::*;

/// The configured host an ssh destination names, if croft knows one (#364).
///
/// Matches on the alias or the `HostName`, either of which people type, and
/// ignores any `user@` prefix — the user is how you log in, not which machine
/// it is, and offering a different workspace per login name would split one
/// box into several.
///
/// `None` for a host with no `~/.ssh/config` entry. That is a real limit and
/// the right one for now: the offer exists to hand the destination to the
/// remote-connect flow, which is keyed on a config entry, so offering for an
/// unknown host would promise something the next step cannot deliver.
pub fn resolve_offer_host<'a>(dest: &str, targets: &'a [RemoteTarget]) -> Option<&'a RemoteTarget> {
    let host = dest.rsplit('@').next().unwrap_or(dest).trim();
    if host.is_empty() {
        return None;
    }
    // Alias first, across ALL entries, before falling back to HostName.
    // A single `find` testing both would let an earlier entry's HostName beat
    // a later entry's own alias: with `Host jump / HostName db-1` above
    // `Host db-1`, typing `ssh db-1` resolved to `jump` — an offer to re-root
    // onto a different machine than the one named.
    targets
        .iter()
        .find(|t| t.alias.eq_ignore_ascii_case(host))
        .or_else(|| {
            targets.iter().find(|t| {
                t.host_name
                    .as_deref()
                    .is_some_and(|h| h.eq_ignore_ascii_case(host))
            })
        })
}

/// The offer's answer, or the reason there is none (#364).
///
/// Split from the app handler so all four outcomes are assertable without a
/// live pane: the handler samples argv and renders this, and nothing else
/// decides what the user is told.
pub fn ssh_reroot_decision<'a>(
    argv: &[&str],
    targets: &'a [RemoteTarget],
) -> Result<&'a RemoteTarget, String> {
    match ssh_destination(argv) {
        Some(dest) => resolve_offer_host(&dest, targets).ok_or_else(|| {
            // An ssh session to a box with no config entry: name it, since
            // that is the actionable half of the answer.
            format!("{dest} is not in your ~/.ssh/config, so croft cannot open a workspace on it")
        }),
        None => Err(String::from("This pane is not an SSH session")),
    }
}

/// Hosts croft learned it cannot provision (#364): lower-cased alias to the
/// time of the failure. The time travels with the entry so the TTL applies
/// to an entry learned during this run as much as to one loaded at startup;
/// a process left open for a week must not suppress a host forever.
pub type RefusedHosts = std::collections::BTreeMap<String, std::time::SystemTime>;

/// Whether a refusal recorded at `at` still counts at `now` (#364): younger
/// than [`REFUSED_HOST_TTL`]. A timestamp in the FUTURE does not count
/// either: it means the clock was corrected backward since the refusal was
/// recorded, and an age that cannot be computed must not read as zero, or
/// the entry would outlive its window by however far the clock moved.
pub fn refusal_live(at: std::time::SystemTime, now: std::time::SystemTime) -> bool {
    now.duration_since(at)
        .is_ok_and(|age| age <= REFUSED_HOST_TTL)
}

/// Whether the ssh-pane workspace offer may prompt for `host` at `now`
/// (#364): not globally disabled, not on the user's per-host opt-out list,
/// and not a host with a live learned refusal. Pure, so the policy is
/// testable without a pane or a filesystem.
pub fn offer_allowed(
    host: &str,
    disabled: bool,
    excluded: &[String],
    refused: &RefusedHosts,
    now: std::time::SystemTime,
) -> bool {
    if disabled {
        return false;
    }
    if excluded.iter().any(|h| h.eq_ignore_ascii_case(host)) {
        return false;
    }
    !refused
        .get(&host.to_ascii_lowercase())
        .is_some_and(|at| refusal_live(*at, now))
}

/// A pane's last OSC 7 report: the directory its shell says it is in, and
/// the host that shell says it runs on.
pub type CwdReport = (std::path::PathBuf, Option<String>);

/// The remote directory to open when the ssh-pane offer for `host` is
/// accepted (#364), from the pane's OSC 7 report now and the one it had when
/// the offer appeared. A report is used only when it comes from another
/// machine and is either new since the offer (the remote shell's first
/// prompt) or names the offered host. A report from this machine, or a stale
/// one left by an earlier session elsewhere, is never taken for the remote's
/// directory; `None` then opens the login directory, as before.
pub fn remote_workspace_path(
    host: &str,
    at_offer: Option<&CwdReport>,
    now: Option<&CwdReport>,
) -> Option<String> {
    let (path, reporter) = now?;
    let reporter = reporter.as_deref().filter(|h| !h.is_empty())?;
    if crate::command_history::is_local_host(reporter) {
        return None;
    }
    let fresh = at_offer != now;
    if !fresh && !same_host(host, reporter) {
        return None;
    }
    Some(path.to_string_lossy().into_owned())
}

/// Whether an ssh alias and a shell-reported hostname name the same machine:
/// equal ignoring case, or one is the other plus a domain (`db-1` and
/// `db-1.internal`). `db-1` and `db-10` are different hosts.
pub(super) fn same_host(alias: &str, reported: &str) -> bool {
    let (a, r) = (alias.to_ascii_lowercase(), reported.to_ascii_lowercase());
    a == r
        || r.strip_prefix(&a).is_some_and(|rest| rest.starts_with('.'))
        || a.strip_prefix(&r).is_some_and(|rest| rest.starts_with('.'))
}

/// The label thread's ssh verdict for each pane it named (#364). `named` is
/// `(shell pid, foreground pid, foreground name)` per pane; the result is
/// `(shell pid, host)` for every pane that has a verdict:
///
/// * a foreground that is the shell itself, or any program but ssh, is a
///   confirmed non-session: `None`, which ends the pane's session;
/// * an ssh whose argv `cmdline` cannot read is NO verdict, so the pane is
///   left out rather than reported as ended: a transient read failure must
///   not tear down an offer and re-raise it on the next sample;
/// * an ssh with argv resolves through [`ssh_offer_host`]: the config
///   alias, or `None` for a host the config does not know (a confirmed
///   non-offerable endpoint).
///
/// `targets` (the `~/.ssh/config` parse) is called at most once, and only
/// when some pane's foreground is ssh.
pub fn ssh_pane_samples(
    named: &[(i32, i32, String)],
    targets: impl FnOnce() -> Vec<RemoteTarget>,
    cmdline: impl Fn(i32) -> Option<Vec<String>>,
) -> Vec<(i32, Option<String>)> {
    let mut targets = Some(targets);
    let mut config: Vec<RemoteTarget> = Vec::new();
    let mut out = Vec::with_capacity(named.len());
    for (shell, fg, name) in named {
        if shell == fg || !is_ssh_program(name) {
            out.push((*shell, None));
            continue;
        }
        let Some(argv) = cmdline(*fg) else {
            continue;
        };
        if let Some(parse) = targets.take() {
            config = parse();
        }
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        out.push((*shell, ssh_offer_host(&argv, &config)));
    }
    out
}

/// Whether a resolved foreground process name is an ssh program (#364): the
/// cheap pre-check before the label thread reads that process's argv, which
/// costs a whole-process-table enumeration through sysinfo. `name` is what
/// `process_name` returns, a bare basename, but a path is tolerated.
pub fn is_ssh_program(name: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name);
    SSH_PROGRAMS.contains(&base)
}

/// The host a pane's foreground argv earns an offer for (#364): the resolved
/// `~/.ssh/config` alias, or `None` when the process is not ssh, names no
/// host yet, or names a host the config does not know.
pub fn ssh_offer_host(argv: &[&str], targets: &[RemoteTarget]) -> Option<String> {
    ssh_reroot_decision(argv, targets)
        .ok()
        .map(|t| t.alias.clone())
}

/// How long a learned "cannot provision" verdict silences the offer for a
/// host (#364). Long enough that a jump host does not nag every session,
/// short enough that a box whose install failed on a bad day is offered
/// again without the user finding and deleting a cache file.
pub const REFUSED_HOST_TTL: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// The file under croft's cache directory that remembers hosts whose
/// provisioning failed (#364): a JSON map of alias (lower-cased) to the unix
/// time of the failure.
pub fn refused_hosts_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("remote-offer-refused.json")
}

/// The hosts remembered as unprovisionable, minus entries whose refusal is
/// no longer live as of `now` (see [`refusal_live`]). A missing or unreadable
/// file is an empty map: this memory only ever suppresses a prompt, so
/// losing it is safe.
pub fn load_refused_hosts(path: &Path, now: std::time::SystemTime) -> RefusedHosts {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Default::default();
    };
    let Ok(map) = serde_json::from_str::<std::collections::BTreeMap<String, u64>>(&raw) else {
        return Default::default();
    };
    map.into_iter()
        .map(|(h, at)| {
            (
                h.to_ascii_lowercase(),
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(at),
            )
        })
        .filter(|(_, at)| refusal_live(*at, now))
        .collect()
}

/// Forget a remembered refusal for `host` (#364): a later provisioning that
/// SUCCEEDED proves the memory stale, and a week is too long to keep not
/// offering a box that merely failed on a bad day. Best effort like the
/// remembering side; a missing file or entry is not an error.
pub fn forget_refused_host(path: &Path, host: &str) -> std::io::Result<()> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let mut map: std::collections::BTreeMap<String, u64> =
        serde_json::from_str(&raw).unwrap_or_default();
    if map.remove(&host.to_ascii_lowercase()).is_none() {
        return Ok(());
    }
    let body = serde_json::to_string_pretty(&map).map_err(std::io::Error::other)?;
    std::fs::write(path, body)
}

/// Record that provisioning `host` failed at `now` (#364), keeping the other
/// entries. Best effort: a write failure costs one repeated offer, not data.
pub fn remember_refused_host(
    path: &Path,
    host: &str,
    now: std::time::SystemTime,
) -> std::io::Result<()> {
    let mut map: std::collections::BTreeMap<String, u64> = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let now_secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    map.insert(host.to_ascii_lowercase(), now_secs);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = serde_json::to_string_pretty(&map).map_err(std::io::Error::other)?;
    std::fs::write(path, body)
}
