//! What the roster says: the notice for clients older than this croft, the version
//! clients are measured against, the one client a Session: Detach can come from, and the
//! PTY size every client can show.

use super::*;

/// The status line for attached clients older than this croft, or `None`
/// when no client that reported a version is older (#626, #652). A client
/// that reported none (an old client) says nothing either way, and neither
/// does a newer one: the first-attach banner already covered it, and
/// reattaching would not help.
pub fn stale_client_notice(roster: &[Participant], own: &str) -> Option<String> {
    let mut older: Vec<(Vec<u64>, &str)> = roster
        .iter()
        .map(|p| p.version.as_str())
        .filter(|v| version_is_older(v, own))
        .filter_map(|v| Some((parse_version(v)?, v)))
        .collect();
    older.sort_unstable();
    older.dedup();
    let older: Vec<&str> = older.into_iter().map(|(_, v)| v).collect();
    (!older.is_empty()).then(|| {
        format!(
            "Session updated to croft {own}; a client still runs {}. Detach it (Cmd+K Shift+Q) and reattach to update",
            older.join(", ")
        )
    })
}

/// The stale-client notice to raise for roster `next`, given the notice
/// `last` raised (or `None`): `None` unless the notice itself changed. A
/// client resizing churns the roster every few hundred milliseconds, and
/// must not re-stamp the status line over whatever the user is reading.
pub fn stale_client_notice_on_change(
    last: Option<&str>,
    next: &[Participant],
    reference: &str,
) -> Option<String> {
    let notice = stale_client_notice(next, reference)?;
    (last != Some(notice.as_str())).then_some(notice)
}

/// The one client that can have asked for Session: Detach from inside the
/// app: the sole control holder (a read-only client's keys never reach
/// croft), or the sole client. `None` when several could have, since the
/// typing attribution can race another writer's keys.
pub fn sole_detach_target(roster: &[Participant]) -> Option<u64> {
    let mut writers = roster.iter().filter(|p| p.control);
    match (writers.next(), writers.next()) {
        (Some(only), None) => Some(only.id),
        (None, None) => match roster {
            [only] => Some(only.id),
            _ => None,
        },
        _ => None,
    }
}

/// The version clients are measured against: the newer of the inner croft's
/// own and the session host's (#652). A host swap updates the host without
/// the inner croft, and a client on the pre-swap binary is still out of date
/// even though it matches the inner croft.
pub fn session_version<'a>(own: &'a str, host: Option<&'a str>) -> &'a str {
    match host {
        Some(h) if version_is_older(own, h) => h,
        _ => own,
    }
}

/// A dotted version as numeric components, or `None` when any component
/// is not a number (empty, or a pre-release suffix).
pub(super) fn parse_version(v: &str) -> Option<Vec<u64>> {
    v.split('.').map(|c| c.parse().ok()).collect()
}

/// True when dotted version `v` sorts strictly below `own`, compared
/// numerically per component. An empty or unparsable `v` is never older.
pub(super) fn version_is_older(v: &str, own: &str) -> bool {
    match (parse_version(v), parse_version(own)) {
        (Some(v), Some(own)) => v < own,
        _ => false,
    }
}

/// The PTY size every connected client can display: minimum cols and minimum
/// rows across clients (the tmux rule). Clients reporting no size (0 in
/// either dimension, seen under `script` and some CI PTYs) are pure
/// observers here and never shrink the shared PTY. None when no client with
/// a usable size is connected.
pub fn min_winsize(sizes: impl Iterator<Item = (u16, u16)>) -> Option<(u16, u16)> {
    sizes
        .filter(|&(c, r)| c > 0 && r > 0)
        .reduce(|(c1, r1), (c2, r2)| (c1.min(c2), r1.min(r2)))
}
