//! Starting outside programs (#2062, card 12).
//!
//! Every git, gh, ssh, rsync and scp call names its program through
//! [`program`], or starts at [`command`]. A test can put a stand-in in a
//! program's place for its own thread with [`stand_in`]. In a release build
//! the name passes straight through, so croft runs exactly the program it
//! named.

use std::ffi::OsString;
use std::process::Command;

/// The program croft starts for `name`: `name` itself, or in a test the
/// stand-in this thread put in its place.
pub fn program(name: &str) -> OsString {
    #[cfg(test)]
    if let Some(path) = STAND_INS.with(|s| s.borrow().get(name).cloned()) {
        return path.into_os_string();
    }
    OsString::from(name)
}

/// A [`Command`] for [`program`]`(name)`.
pub fn command(name: &str) -> Command {
    Command::new(program(name))
}

#[cfg(test)]
thread_local! {
    /// The stand-ins this test thread put in place of programs, by name.
    static STAND_INS: std::cell::RefCell<std::collections::HashMap<String, std::path::PathBuf>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Start `path` wherever code on this thread starts `name`, for the rest of
/// the thread. Commands built on other threads still start `name`.
#[cfg(test)]
pub(crate) fn stand_in(name: &str, path: std::path::PathBuf) {
    STAND_INS.with(|s| s.borrow_mut().insert(name.to_string(), path));
}

/// A stand-in script for `name` on this thread that runs `body` with `sh`.
/// It is ready to run when this returns: a script just written stays "Text
/// file busy" while another test thread's forked child still holds its write
/// descriptor, so a dry run waits that out before any code under test starts
/// it, including code that starts a program without retrying.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn stand_in_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!("#!/bin/sh\n[ \"$1\" = --stand-in-ready ] && exit 0\n{body}"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let ready = crate::review_ops::retry_while_busy(
        || {
            Command::new(&path)
                .arg("--stand-in-ready")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
        },
        |r| r.as_ref().is_err_and(crate::review_ops::text_file_busy),
    );
    assert!(
        ready.as_ref().is_ok_and(|s| s.success()),
        "the stand-in for {name} runs: {ready:?}"
    );
    stand_in(name, path.clone());
    path
}

/// What a [`record_runs`] stand-in wrote down.
#[cfg(all(test, target_os = "linux"))]
pub(crate) struct Runs {
    log: std::path::PathBuf,
}

#[cfg(all(test, target_os = "linux"))]
impl Runs {
    /// One line per run: its arguments, then where its stdin, stdout and
    /// stderr went. `/dev/null` is nowhere; a pipe reads `pipe`, a socket
    /// `socket` and a terminal `terminal`, whichever one it was.
    pub(crate) fn lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| {
                line.split(' ')
                    .map(|word| {
                        if word.starts_with("pipe:[") {
                            "pipe"
                        } else if word.starts_with("socket:[") {
                            "socket"
                        } else if word.starts_with("/dev/pts/") || word.starts_with("/dev/tty") {
                            "terminal"
                        } else {
                            word
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }
}

/// A stand-in for `name` on this thread that does nothing but write down
/// each run: its arguments, and what each of its three streams is connected
/// to, read from `/proc`. It exits 0.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn record_runs(dir: &std::path::Path, name: &str) -> Runs {
    let log = dir.join(format!("{name}.runs"));
    stand_in_script(
        dir,
        name,
        &format!(
            "i=$(readlink /proc/$$/fd/0); o=$(readlink /proc/$$/fd/1); e=$(readlink /proc/$$/fd/2)\n\
             printf '%s | stdin %s | stdout %s | stderr %s\\n' \"$*\" \"$i\" \"$o\" \"$e\" >> '{}'\n",
            log.display()
        ),
    );
    Runs { log }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_passes_through_until_a_stand_in_takes_its_place() {
        assert_eq!(program("git"), OsString::from("git"));
        stand_in("git", std::path::PathBuf::from("/stand-in/git"));
        assert_eq!(program("git"), OsString::from("/stand-in/git"));
        assert_eq!(
            command("git").get_program(),
            std::ffi::OsStr::new("/stand-in/git")
        );
        // Other names, and other threads, are untouched.
        assert_eq!(program("ssh"), OsString::from("ssh"));
        let elsewhere = std::thread::spawn(|| program("git")).join().unwrap();
        assert_eq!(elsewhere, OsString::from("git"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_recording_stand_in_writes_down_each_run_and_its_streams() {
        let dir = tempfile::tempdir().unwrap();
        let runs = record_runs(dir.path(), "tool");
        let quiet = command("tool")
            .args(["-x", "one"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(quiet.success());
        let piped = command("tool").arg("two").output().unwrap();
        assert!(piped.status.success());
        assert_eq!(
            runs.lines(),
            [
                "-x one | stdin /dev/null | stdout /dev/null | stderr /dev/null",
                "two | stdin /dev/null | stdout pipe | stderr pipe",
            ]
        );
    }
}
