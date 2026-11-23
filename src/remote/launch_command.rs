//! The command line that starts croft on the remote, and the session, mux and collab
//! sockets it names.

use super::*;

/// A value unique to THIS client process, for the session-host client
/// identity (#229). Call once per process and carry the result; calling it
/// twice yields different values by design.
///
/// pid alone is not enough — pids are recycled, and two croft clients on
/// different machines attaching to one remote session can share one. Mixing
/// in the start time and the address of a local allocation distinguishes
/// those without needing a uuid dependency.
pub(crate) fn client_process_nonce() -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::process::id().hash(&mut hasher);
    if let Ok(since_epoch) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        since_epoch.as_nanos().hash(&mut hasher);
    }
    // ASLR plus allocator state: distinct between concurrent processes that
    // started in the same nanosecond and share a recycled pid.
    let here = Box::new(0u8);
    (Box::as_ref(&here) as *const u8 as usize).hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub fn remote_croft_command(path: Option<&str>, env: &[(String, String)], solo: bool) -> String {
    remote_croft_command_for_terminal(
        path,
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
        std::env::var(crate::iterm2_inline::CROFT_INLINE_IMAGES_ENV)
            .ok()
            .as_deref(),
        crate::iterm2_inline::detect_osk_auto(),
        &with_underline_colors(
            env,
            crate::color_depth::underline_colors_supported(|name| std::env::var(name).ok()),
        ),
        solo,
    )
}

/// `env` led by whether this terminal reads underline colours (#2661). The
/// remote croft draws into this same terminal but cannot see the variables
/// that name it, so it is told; an entry in `env` still wins, coming later.
pub(super) fn with_underline_colors(
    env: &[(String, String)],
    supported: bool,
) -> Vec<(String, String)> {
    let mut out = vec![(
        String::from(crate::color_depth::UNDERLINE_COLORS_ENV),
        String::from(if supported { "1" } else { "0" }),
    )];
    out.extend_from_slice(env);
    out
}

pub(super) fn remote_croft_command_for_terminal(
    path: Option<&str>,
    term_program: Option<&str>,
    term: Option<&str>,
    croft_inline_images: Option<&str>,
    osk: bool,
    env: &[(String, String)],
    solo: bool,
) -> String {
    use crate::iterm2_inline::InlineImageProtocol;
    let mut prefix = String::from("export CROFT_REMOTE_AUTOUPDATE=1; ");
    // SSH from Termux does not forward TERMUX_VERSION, so the remote croft
    // can't detect the touch environment itself; carry the local on-screen
    // keyboard detection across the hop like the TERM_PROGRAM hint below.
    if osk {
        prefix.push_str("export CROFT_FORCE_OSK=1; ");
    }
    // The remote croft renders into *this* terminal over the SSH PTY, so it has
    // to use whichever inline-image protocol the local terminal speaks. SSH
    // forwards `TERM` but not `TERM_PROGRAM` (absent an `AcceptEnv` opt-in), so
    // export the hint the remote detector needs explicitly rather than relying
    // on the environment surviving the hop. Launched from a croft pane whose
    // croft draws no images, the remote must not be forced onto OSC 1337 it
    // cannot show (#1453), so the pane's `CROFT_INLINE_IMAGES` gate applies.
    match crate::iterm2_inline::inline_image_protocol_in(term_program, term, croft_inline_images) {
        InlineImageProtocol::ITerm2 => {
            // iTerm2/WezTerm implement OSC 1337 inline images + `SetColors`;
            // force them on and carry `TERM_PROGRAM` so the remote paints the
            // session background too.
            let tp = term_program.unwrap_or("iTerm.app");
            prefix.push_str("export CROFT_FORCE_INLINE_IMAGES=1 TERM_PROGRAM=");
            prefix.push_str(&shell_quote(tp));
            prefix.push_str("; ");
        }
        InlineImageProtocol::Kitty => {
            // Ghostty/Kitty render via the Kitty graphics protocol and paint the
            // background with explicit truecolor (no `SetColors`), so export
            // `TERM_PROGRAM=ghostty` only; do NOT force the OSC-1337 path.
            prefix.push_str("export TERM_PROGRAM=ghostty; ");
        }
        // Sixel is never returned by `inline_image_protocol_for` (it has no env
        // signal); the remote croft discovers it by running its own DA1 probe
        // against this same terminal over the SSH PTY, so no hint is forwarded.
        InlineImageProtocol::Sixel => {}
        InlineImageProtocol::None => {}
    }
    for (k, v) in env {
        prefix.push_str("export ");
        prefix.push_str(k);
        prefix.push('=');
        prefix.push_str(&shell_quote(v));
        prefix.push_str("; ");
    }
    prefix.push_str("export PATH=\"$HOME/.cargo/bin:$PATH\"; command -v croft >/dev/null 2>&1 || { echo 'croft not found on remote PATH' >&2; exit 127; }; ");
    let croft_invocation = match path.filter(|p| !p.is_empty()) {
        Some(p) => format!("croft {}", shell_quote(p)),
        None => String::from("croft"),
    };
    // A solo guest never attaches the shared PTY: it runs its own croft
    // (independent viewport) wired to the workspace's collab relay for
    // shared-file edits (docs/MULTIPLAYER.md, Phase D). The probe keeps a
    // freshly shipped local croft from driving an unknown subcommand at a
    // remote binary the background updater hasn't replaced yet, exactly
    // like the session-host probe below; such a host falls back to a plain
    // (non-collab) croft rather than failing the connect. No persistence
    // supervisor: a solo viewport is per-guest scratch state, and wrapping
    // it in the mux would just share its PTY again.
    if solo {
        let collab_socket = collab_socket_path(path);
        return format!(
            "{prefix}if croft collab-relay --probe >/dev/null 2>&1; then mkdir -p \"$(dirname \"{collab_socket}\")\"; croft collab-relay --ensure --socket \"{collab_socket}\"; export CROFT_COLLAB_SOCKET=\"{collab_socket}\"; export CROFT_COLLAB_ROLE=guest; exec {croft_invocation}; else exec {croft_invocation}; fi"
        );
    }
    // Run croft under a persistence supervisor so the session survives an SSH
    // transport drop (laptop sleep, network change). Preferred: croft's own
    // session host (`croft session-host`, the multiplayer mux; see
    // docs/MULTIPLAYER.md) — attach-or-create semantics, byte-transparent
    // broadcast to every attached client, server-side write control, and it
    // propagates the inner exit code (dtach never did, so drop-to-local's 88
    // only works on this branch). The `--probe` guard keeps a freshly shipped
    // local croft from sending an unknown subcommand at a remote binary the
    // background updater hasn't replaced yet. Fallback: dtach, exactly as
    // before (`-A` attach-or-create, `-E`/`-z` keep its hands off croft's
    // Ctrl chords, `-r winch` repaints on reattach). Both supervisors are
    // byte-transparent so OSC-1337 / Kitty graphics pass through untouched —
    // tmux corrupts the Kitty protocol, which is why neither branch uses it.
    // Hosts with neither exec croft directly, no persistence.
    // `CROFT_SESSION_PERSISTENT=1` is exported only on the persistent
    // branches so the remote croft can tell whether its session survives a
    // transport drop and surface that on its status line. Under dtach a
    // later attach may come from a terminal that cannot read underline
    // colours and nothing can remove them on its way (the session host's
    // clients do, #2661), so that branch never draws them.
    let socket = dtach_socket_path(path);
    let mux_socket = mux_socket_path(path);
    let workspace_flag = match path.filter(|p| !p.is_empty()) {
        Some(p) => format!(" --workspace {}", shell_quote(p)),
        None => String::new(),
    };
    format!(
        "{prefix}if croft session-host --probe >/dev/null 2>&1; then mkdir -p \"$(dirname \"{mux_socket}\")\"; export CROFT_SESSION_PERSISTENT=1; exec croft session-host --socket \"{mux_socket}\"{workspace_flag} -- {croft_invocation}; elif command -v dtach >/dev/null 2>&1; then mkdir -p \"$(dirname \"{socket}\")\"; export CROFT_SESSION_PERSISTENT=1 CROFT_UNDERLINE_COLORS=0; exec dtach -A \"{socket}\" -E -z -r winch {croft_invocation}; else exec {croft_invocation}; fi"
    )
}

/// Remote path of the dtach control socket for a workspace. Keyed on a stable
/// hash of the workspace path so a reconnect reattaches the same session.
/// `DefaultHasher` uses fixed SipHash keys, so the name is deterministic across
/// croft processes (a randomized hasher would orphan the session on relaunch).
pub(super) fn dtach_socket_path(path: Option<&str>) -> String {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(path.unwrap_or("").as_bytes());
    format!("$HOME/.cache/croft/sessions/{:016x}.sock", hasher.finish())
}

/// Remote socket for a mux (session-host) session: same keying as the dtach
/// socket (the raw launch arg, per the relay-key post-mortem above
/// `relay_session_id`) but a distinct name, so a mux client never speaks
/// croft's frame protocol at a live dtach server left over from an older
/// binary.
pub(super) fn mux_socket_path(path: Option<&str>) -> String {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(path.unwrap_or("").as_bytes());
    format!(
        "$HOME/.cache/croft/sessions/{:016x}.mux.sock",
        hasher.finish()
    )
}

/// Remote socket carrying Phase D collab ops between independent-viewport
/// participants (never PTY bytes): same keying as the dtach and mux sockets,
/// its own endpoint (see docs/MULTIPLAYER.md).
pub(super) fn collab_socket_path(path: Option<&str>) -> String {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(path.unwrap_or("").as_bytes());
    format!(
        "$HOME/.cache/croft/sessions/{:016x}.collab.sock",
        hasher.finish()
    )
}
