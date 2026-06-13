//! PEP 768 remote-attach planning.
//!
//! Pure, side-effect-free logic for the "attach a `pdb` REPL to a running
//! Python process" command: parsing/gating the target interpreter version and
//! constructing the (possibly `sudo`-wrapped) command line to spawn in a PTY.
//! The impure parts (enumerating processes, reading `ptrace_scope`, spawning the
//! PTY) live in the app layer and call into these helpers.

use std::path::PathBuf;

/// CPython's `pdb` module, invoked with `-m`.
const PDB_MODULE: &str = "pdb";
/// `pdb`'s attach-to-pid flag (PEP 768, Python 3.14+).
const PDB_ATTACH_FLAG: &str = "-p";
/// Elevation wrapper used on both platforms when attaching to a process.
const SUDO: &str = "sudo";

/// Lowest CPython version exposing `sys.remote_exec` (PEP 768).
pub const MIN_ATTACH_VERSION: PyVersion = PyVersion {
    major: 3,
    minor: 14,
    patch: 0,
};

/// A parsed CPython version. Ordered, so `>=` comparisons work directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PyVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl PyVersion {
    /// Parse from `python --version` output (`"Python 3.14.2"`) or a bare
    /// version string (`"3.14.2"`, `"3.14"`). Pre-release suffixes on the patch
    /// component (`"3.14.0a5"`, `"3.15.0rc1"`) are tolerated: the leading digits
    /// are taken and the suffix dropped. Returns `None` if no `major.minor` can
    /// be found.
    pub fn parse(s: &str) -> Option<PyVersion> {
        // Take the first whitespace-delimited token that starts with a digit,
        // so both "Python 3.14.2" and "3.14.2" work.
        let token = s
            .split_whitespace()
            .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()))?;

        let mut parts = token.split('.');
        let major = leading_u32(parts.next()?)?;
        let minor = leading_u32(parts.next()?)?;
        // Patch is optional; a pre-release suffix (a/b/rc) is stripped.
        let patch = parts.next().and_then(leading_u32).unwrap_or(0);

        Some(PyVersion {
            major,
            minor,
            patch,
        })
    }

    /// True when this interpreter exposes `sys.remote_exec` and can therefore be
    /// attached to. The gate is `(major, minor) >= (3, 14)`; the patch level is
    /// not consulted (pre-release alphas are not shipped to end users).
    pub fn supports_remote_attach(&self) -> bool {
        (self.major, self.minor) >= (MIN_ATTACH_VERSION.major, MIN_ATTACH_VERSION.minor)
    }
}

/// Parse the leading run of ASCII digits in `s` as a `u32`, ignoring any
/// trailing non-digit suffix (e.g. `"0a5"` -> `0`).
fn leading_u32(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Host platform, for the elevation decision. Kept explicit (rather than reading
/// `cfg!`) so the parity-critical branch is unit-testable for both targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
}

impl Platform {
    /// The platform croft is compiled for.
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
        }
    }
}

/// Whether attaching to a same-user process requires elevation.
///
/// GOLDEN-RULE note: both platforms need elevation, so the *observable* behavior
/// (croft prompts for a password in the PTY) is identical; only the trigger
/// differs.
///
/// - **macOS**: always required. The kernel's task-port restriction blocks
///   cross-process debugging even for processes you own, with no user-facing
///   entitlement/`task_for_pid` workaround (official 3.14 howto).
/// - **Linux**: required unless Yama `ptrace_scope` is fully relaxed (`0`). A
///   `scope` of `1` (default), `2`, `3`, or an unreadable value (`None`) all
///   require elevation to be safe.
pub fn elevation_required(platform: Platform, yama_ptrace_scope: Option<u8>) -> bool {
    match platform {
        Platform::MacOs => true,
        Platform::Linux => !matches!(yama_ptrace_scope, Some(0)),
    }
}

/// Read Linux's Yama `ptrace_scope` (`/proc/sys/kernel/yama/ptrace_scope`),
/// the value [`elevation_required`] consults. Returns `None` on macOS (no such
/// file) or if the file is missing/unreadable/unparseable, which
/// [`elevation_required`] treats as "elevation needed" (the safe default).
pub fn read_yama_ptrace_scope() -> Option<u8> {
    std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope")
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// A fully-resolved plan to attach `pdb` to a running process, ready to spawn in
/// a croft PTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachPlan {
    /// A CPython 3.14+ interpreter to run `-m pdb -p` (the pdb *client*).
    pub interpreter: PathBuf,
    /// Target process id.
    pub pid: u32,
    /// Wrap the command in `sudo` (see [`elevation_required`]).
    pub elevate: bool,
}

impl AttachPlan {
    /// The `(program, args)` to hand to `PtyTerminal::new_running`. When
    /// elevation is needed the program becomes `sudo` and the interpreter moves
    /// into the argument list, so the interactive `sudo` password prompt is
    /// hosted by the same PTY as the pdb session.
    pub fn command_line(&self) -> (String, Vec<String>) {
        let interp = self.interpreter.to_string_lossy().into_owned();
        let pid = self.pid.to_string();
        let pdb_args = [
            "-m".to_string(),
            PDB_MODULE.to_string(),
            PDB_ATTACH_FLAG.to_string(),
            pid,
        ];

        if self.elevate {
            let mut args = Vec::with_capacity(pdb_args.len() + 1);
            args.push(interp);
            args.extend(pdb_args);
            (SUDO.to_string(), args)
        } else {
            (interp, pdb_args.to_vec())
        }
    }
}

/// Why an attach could not be planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachError {
    /// The target process runs a CPython older than 3.14, which has no
    /// `sys.remote_exec`, so it cannot be attached to.
    UnsupportedTarget(PyVersion),
}

/// Compose the attach decision: gate on the target's interpreter version, then
/// resolve whether the spawn must be elevated for this platform. The
/// `interpreter` is the 3.14+ CPython used to run `-m pdb -p` (the pdb client).
pub fn plan_attach(
    target_version: PyVersion,
    interpreter: PathBuf,
    pid: u32,
    platform: Platform,
    yama_ptrace_scope: Option<u8>,
) -> Result<AttachPlan, AttachError> {
    if !target_version.supports_remote_attach() {
        return Err(AttachError::UnsupportedTarget(target_version));
    }
    Ok(AttachPlan {
        interpreter,
        pid,
        elevate: elevation_required(platform, yama_ptrace_scope),
    })
}

/// Whether a process is a CPython interpreter worth offering as an attach
/// target, judged from its process name and (optional) executable path.
///
/// Matches `python`, `python3`, `python3.14`, `Python` (the macOS framework
/// name) and a venv's bare `python`, while rejecting look-alikes: a script
/// merely *named* like python (`my_python_tool`, `pythonista`) or another
/// program. The basename must equal `python` or begin with `python` followed by
/// a digit or `.`.
pub fn is_python_process(name: &str, exe: Option<&std::path::Path>) -> bool {
    let basename = exe
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let lower = basename.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);

    if stem == "python" {
        return true;
    }
    if let Some(rest) = stem.strip_prefix("python") {
        // "python3", "python3.14" -> next char is a digit; reject "pythonista".
        return rest.starts_with(|c: char| c.is_ascii_digit());
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parses_python_version_banner() {
        assert_eq!(
            PyVersion::parse("Python 3.14.2"),
            Some(PyVersion {
                major: 3,
                minor: 14,
                patch: 2
            })
        );
    }

    #[test]
    fn parses_bare_version_and_two_component_version() {
        assert_eq!(
            PyVersion::parse("3.13.2"),
            Some(PyVersion {
                major: 3,
                minor: 13,
                patch: 2
            })
        );
        assert_eq!(
            PyVersion::parse("3.14"),
            Some(PyVersion {
                major: 3,
                minor: 14,
                patch: 0
            })
        );
    }

    #[test]
    fn tolerates_prerelease_suffix_on_patch() {
        assert_eq!(
            PyVersion::parse("Python 3.14.0a5"),
            Some(PyVersion {
                major: 3,
                minor: 14,
                patch: 0
            })
        );
    }

    #[test]
    fn rejects_non_version_strings() {
        assert_eq!(PyVersion::parse(""), None);
        assert_eq!(PyVersion::parse("not a version"), None);
        assert_eq!(PyVersion::parse("python"), None);
    }

    #[test]
    fn remote_attach_gate_is_3_14() {
        // 3.13 cannot be attached to; 3.14+ can. Proven empirically 2026-06-13.
        assert!(!PyVersion::parse("3.13.2").unwrap().supports_remote_attach());
        assert!(PyVersion::parse("3.14.0").unwrap().supports_remote_attach());
        assert!(PyVersion::parse("3.14.2").unwrap().supports_remote_attach());
        assert!(PyVersion::parse("3.15.0").unwrap().supports_remote_attach());
        assert!(PyVersion::parse("4.0.0").unwrap().supports_remote_attach());
    }

    #[test]
    fn macos_always_requires_elevation() {
        // Regardless of any ptrace value, macOS needs root.
        assert!(elevation_required(Platform::MacOs, None));
        assert!(elevation_required(Platform::MacOs, Some(0)));
        assert!(elevation_required(Platform::MacOs, Some(1)));
    }

    #[test]
    fn linux_elevation_follows_ptrace_scope() {
        assert!(!elevation_required(Platform::Linux, Some(0))); // fully relaxed
        assert!(elevation_required(Platform::Linux, Some(1))); // default
        assert!(elevation_required(Platform::Linux, Some(2)));
        assert!(elevation_required(Platform::Linux, Some(3)));
        assert!(elevation_required(Platform::Linux, None)); // unreadable -> safe
    }

    #[test]
    fn command_line_without_elevation_runs_interpreter_directly() {
        let plan = AttachPlan {
            interpreter: PathBuf::from("/usr/bin/python3.14"),
            pid: 4242,
            elevate: false,
        };
        let (program, args) = plan.command_line();
        assert_eq!(program, "/usr/bin/python3.14");
        assert_eq!(args, vec!["-m", "pdb", "-p", "4242"]);
    }

    #[test]
    fn command_line_with_elevation_wraps_in_sudo() {
        let plan = AttachPlan {
            interpreter: PathBuf::from("/opt/py/python3.14"),
            pid: 99,
            elevate: true,
        };
        let (program, args) = plan.command_line();
        assert_eq!(program, "sudo");
        assert_eq!(args, vec!["/opt/py/python3.14", "-m", "pdb", "-p", "99"]);
    }

    #[test]
    fn plan_attach_rejects_pre_3_14_target() {
        let old = PyVersion::parse("3.13.2").unwrap();
        let err = plan_attach(
            old,
            PathBuf::from("/x/python3.14"),
            7,
            Platform::Linux,
            Some(0),
        )
        .unwrap_err();
        assert_eq!(err, AttachError::UnsupportedTarget(old));
    }

    #[test]
    fn plan_attach_macos_elevates_even_with_supported_target() {
        let v = PyVersion::parse("3.14.2").unwrap();
        let plan =
            plan_attach(v, PathBuf::from("/x/python3.14"), 7, Platform::MacOs, None).unwrap();
        assert!(plan.elevate);
        assert_eq!(plan.pid, 7);
    }

    #[test]
    fn plan_attach_linux_relaxed_ptrace_skips_sudo() {
        let v = PyVersion::parse("3.14.2").unwrap();
        let plan = plan_attach(
            v,
            PathBuf::from("/x/python3.14"),
            7,
            Platform::Linux,
            Some(0),
        )
        .unwrap();
        assert!(!plan.elevate);
    }

    #[test]
    fn recognizes_python_interpreters() {
        assert!(is_python_process("python", None));
        assert!(is_python_process("python3", None));
        assert!(is_python_process("python3.14", None));
        assert!(is_python_process("Python", None)); // macOS framework name
        // Path basename wins over the (possibly generic) process name.
        assert!(is_python_process(
            "Python",
            Some(Path::new("/Library/Frameworks/Python.framework/.../Python"))
        ));
        assert!(is_python_process(
            "anything",
            Some(Path::new("/usr/bin/python3.14"))
        ));
    }

    #[test]
    fn rejects_python_lookalikes() {
        assert!(!is_python_process("pythonista", None));
        assert!(!is_python_process("my_python_tool", None));
        assert!(!is_python_process("node", None));
        assert!(!is_python_process(
            "python",
            Some(Path::new("/usr/bin/node"))
        ));
    }
}
