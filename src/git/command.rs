//! How croft starts git (#2062, card 12 step 6).
//!
//! Every git call builds through [`Git`] and says by name which of git's
//! hardening it asks for, instead of repeating an environment variable or a
//! pre-exec hook by hand: [`Git::without_optional_locks`] for a read-only
//! query, which must never take `.git/index.lock` from under a change the
//! user is making, and [`Git::never_prompting`] for a call that may reach a
//! remote, which must never stop on a password prompt nobody can see. A call
//! that asks for neither shows that too.
//!
//! The arguments come out in the order the builder is called, so a call
//! built here hands git exactly what it was handed before. The saved record
//! of every git call (`src/git/recorded_calls.rs`) proves it.

use std::ffi::OsStr;
use std::process::Command;

/// One git call, built up before it runs.
pub(crate) struct Git(Command);

impl Git {
    /// `git`, with nothing asked of it yet. Named through
    /// [`crate::proc::command`], so a test can put a stand-in in its place.
    pub(crate) fn new() -> Self {
        Self(crate::proc::command("git"))
    }

    /// Run as if started in `dir`: `-C dir`.
    pub(crate) fn at(mut self, dir: impl AsRef<OsStr>) -> Self {
        self.0.arg("-C").arg(dir);
        self
    }

    /// Never take a lock that only speeds git up (`GIT_OPTIONAL_LOCKS=0`).
    /// `status` and `diff` rewrite a refreshed index when they can, and that
    /// background lock raced the user's own `git add` or `git apply --cached`
    /// into "index.lock exists" failures. Every read-only query asks for this.
    pub(crate) fn without_optional_locks(mut self) -> Self {
        self.0.env("GIT_OPTIONAL_LOCKS", "0");
        self
    }

    /// Never prompt: no terminal prompt, an empty stdin, and a session of its
    /// own, apart from croft's terminal ([`super::never_prompt`]). A push or
    /// pull needing a password or passphrase drew over the full-screen UI and
    /// waited for input croft never passed on; now it fails at once with a
    /// message the panel shows, and credential helpers and agents still work.
    pub(crate) fn never_prompting(mut self) -> Self {
        super::never_prompt(&mut self.0);
        self
    }

    /// More arguments, in order.
    pub(crate) fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.0.args(args);
        self
    }

    /// Environment variables for git.
    pub(crate) fn envs<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.0.envs(vars);
        self
    }

    /// The command, for the call to connect its streams and run as it needs.
    pub(crate) fn into_command(self) -> Command {
        self.0
    }
}
