use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::hash::Hasher;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

mod clipboard;
mod config_push;
mod control;
mod cross_build;
mod forward;
mod hosts;
mod install;
mod install_stream;
mod launch_command;
mod offer;
mod pull;
mod relay;
mod session;
mod source;
pub(crate) use clipboard::MAX_COPY_PAYLOAD_BYTES;
use clipboard::*;
pub use config_push::SyncResolution;
pub use config_push::sync_config_now;
use config_push::*;
pub use control::AdoptedMaster;
pub use control::close_master;
pub use control::ssh_control_dir;
use control::*;
pub(crate) use cross_build::cross_build_static;
pub(crate) use cross_build::cross_build_unavailable;
pub(crate) use cross_build::raise_fd_limit;
use cross_build::*;
use forward::*;
pub use hosts::RemoteTarget;
pub use hosts::SshConfigState;
pub use hosts::discover_ssh_targets;
pub use hosts::primary_ssh_config_path;
pub use hosts::ssh_config_state;
use hosts::*;
pub(crate) use install::FORCE_SOURCE_BUILD;
pub use install::REMOTE_COMPILE_SIDE_EFFECTS;
pub use install::arch_to_musl_triple;
use install::*;
pub use install_stream::install_only_streaming;
use install_stream::*;
pub(crate) use launch_command::client_process_nonce;
use launch_command::*;
pub use offer::CwdReport;
#[cfg(test)]
pub use offer::REFUSED_HOST_TTL;
pub use offer::RefusedHosts;
pub use offer::forget_refused_host;
pub use offer::load_refused_hosts;
pub use offer::offer_allowed;
pub use offer::refused_hosts_path;
pub use offer::remember_refused_host;
pub use offer::remote_workspace_path;
pub use offer::ssh_pane_samples;
pub use offer::ssh_reroot_decision;
#[cfg(test)]
use offer::*;
use pull::*;
use relay::*;
pub use session::DROP_TO_LOCAL_EXIT_CODE;
pub use session::RemoteOutcome;
pub use session::launch_croft;
pub use session::launch_croft_with;
pub use session::launch_only;
pub use session::running_over_ssh;
#[cfg(test)]
use session::*;
pub(crate) use source::shell_quote_for_e_arg;
use source::*;

pub struct SshControl {
    host: String,
    socket_dir: PathBuf,
    socket_path: PathBuf,
}

#[cfg(test)]
thread_local! {
    /// A stand-in for `ssh` in this test thread, so the commands built here
    /// can run against a local shim.
    static TEST_SSH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// The user's config-sync exclusions (#603) as a live re-push reads them.
///
/// Read again whenever a file changes, so an edited exclusion applies
/// without a reconnect. Only a clean read counts: one that skipped a layer
/// (a config file half-written or invalid at that moment) would report the
/// default, empty lists, and pushing on those would send a file to a host
/// the user had excluded. So a bad read keeps the last clean one, and until
/// there has been a clean read at all the exclusions are unknown and
/// nothing is pushed.
struct SyncExclusions {
    /// `(config_sync_excluded_hosts, config_sync_excluded_files)` from the
    /// last clean read; `None` before the first.
    known: Option<(Vec<String>, Vec<String>)>,
}

/// What a live re-push carries between polls.
struct RepushState {
    watch: crate::config_sync::ConfigWatch,
    /// Changed files not yet pushed: a failed push stays here and is
    /// retried, so an edit made while the connection was down still lands
    /// after the reconnect.
    pending: std::collections::BTreeSet<PathBuf>,
    retry_at: Option<std::time::Instant>,
}

/// What a re-push did not send: files that failed to transfer (tried
/// again), and files held because they were edited on the remote since
/// croft last pushed them (#262), which wait for the user to settle.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct PushResult {
    failed: Vec<&'static str>,
    held: Vec<&'static str>,
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests;

mod config_repush;
#[cfg(test)]
mod offer_tests;
use config_repush::*;
