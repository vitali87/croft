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
}
