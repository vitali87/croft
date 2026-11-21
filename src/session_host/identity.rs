//! Who a client is: the user@host name the roster shows, and the stable identity that
//! lets a reconnect displace its own earlier registration.

/// `user@host`, the identity other participants see in the roster (and the
/// default name a collab caret broadcasts).
pub(crate) fn client_name() -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| String::from("user"));
    format!("{user}@{}", hostname())
}

/// Stable identity for this attaching client process (#229), used by the
/// host to displace the same client's earlier, now-dead registration.
///
/// The requirement is narrow and two-sided: it must be IDENTICAL across
/// reconnects of one client, and DISTINCT between genuinely separate
/// clients — two terminals on the same machine attaching to one session are
/// two real participants and neither may evict the other.
///
/// `CROFT_RELAY_KEY` alone satisfies only the first half. It is
/// `hash(launch arg)` (`remote::relay_session_id`), so two terminals running
/// `croft remote host /same/path` derive the SAME key, and the second would
/// evict the first — turning a ghost fix into a way to kick a colleague off.
/// The launch key is therefore combined with `CROFT_CLIENT_NONCE`, which
/// `remote::run_croft_session` mints once per client process, outside its
/// reconnect loop: constant across every reattach that loop performs, and
/// different in any other process.
///
/// Off the remote path both are absent and this returns empty, which
/// displaces nothing. That is deliberate: a local attach has no ghost
/// problem, because a dead unix socket reports EPIPE at once and is pruned
/// on the next broadcast.
pub(crate) fn client_identity() -> String {
    compose_client_identity(
        &std::env::var("CROFT_RELAY_KEY").unwrap_or_default(),
        &std::env::var("CROFT_CLIENT_NONCE").unwrap_or_default(),
    )
}

/// The composition rule behind [`client_identity`], split out so it can be
/// tested directly: `set_var` races sibling test threads (#37), so the env
/// read stays a one-liner and the logic worth asserting lives here.
pub(super) fn compose_client_identity(relay_key: &str, nonce: &str) -> String {
    if relay_key.is_empty() || nonce.is_empty() {
        return String::new();
    }
    format!("{relay_key}.{nonce}")
}

#[cfg(unix)]
pub(super) fn hostname() -> String {
    let mut buf = [0u8; 256];
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    if ok {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    } else {
        String::from("local")
    }
}

#[cfg(not(unix))]
pub(super) fn hostname() -> String {
    String::from("local")
}
