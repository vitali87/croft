//! Debugging subsystem.
//!
//! croft's debugger is a DAP (Debug Adapter Protocol) client in spirit, but the
//! first capability shipped here is the non-DAP **remote attach** path built on
//! CPython's PEP 768 (`sys.remote_exec`, Python 3.14+): croft can drop a live
//! `pdb` REPL into an already-running Python process by spawning
//! `python -m pdb -p <pid>` inside a real croft PTY. The PTY is what makes this
//! cleaner than a GUI debugger: it natively hosts both the interactive pdb
//! prompt and the `sudo` password prompt that attaching requires.
//!
//! Empirically verified on 2026-06-13: `sys.remote_exec` exists on CPython
//! 3.14.2 (and `pdb._PdbServer`/`pdb._connect`, the socket bridge `-m pdb -p`
//! uses) but is absent on 3.13.2 and on the 3.14.0a5 alpha. Attaching to a
//! same-user process needs elevation on **both** platforms: macOS always
//! requires root (task-port restriction, no entitlement workaround per the
//! official 3.14 howto), and Linux requires `CAP_SYS_PTRACE`, typically gated by
//! Yama `ptrace_scope`. The elevation decision is therefore platform-aware; see
//! [`remote_attach::elevation_required`].

pub mod discovery;
pub mod remote_attach;
