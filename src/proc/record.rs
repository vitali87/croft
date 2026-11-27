//! Recorded calls (#2062, card 12): a stand-in that writes down everything
//! croft hands a program it starts, then runs the real program, so a saved
//! record can prove that a change to how croft starts programs changes
//! nothing a program sees.
//!
//! Each run is written down in full: its arguments, the environment
//! variables croft set or removed, its folder, what each of its three
//! streams is connected to, and whether it runs in a session or process
//! group of its own. All of it is read from `/proc`, so this is Linux only.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Every run a [`record`] stand-in saw, taken in the order the runs started.
pub(crate) struct Recording {
    runs: PathBuf,
    name: String,
    /// The test's environment when recording began. A variable another test
    /// changes meanwhile is left out of every run's changes, since it cannot
    /// be told apart from one croft set.
    env_before: BTreeMap<OsString, OsString>,
    taken: usize,
}

/// Put a stand-in for `name` on this thread that writes down each run, then
/// runs `then`: a shell command, which sees the run's arguments as `"$@"`.
pub(crate) fn record(dir: &Path, name: &str, then: &str) -> Recording {
    let runs = dir.join(format!("{name}.recorded"));
    std::fs::create_dir_all(&runs).unwrap();
    recording_stand_in(dir, name, &runs, "", then);
    Recording {
        runs,
        name: name.to_string(),
        env_before: std::env::vars_os().collect(),
        taken: 0,
    }
}

/// The stand-in script for `name` that writes each run down in `runs`, and
/// `mark` beside it, then runs `then`.
fn recording_stand_in(dir: &Path, name: &str, runs: &Path, mark: &str, then: &str) {
    let r = runs.display();
    super::stand_in_script(
        dir,
        name,
        &format!(
            "i=$(readlink /proc/$$/fd/0); o=$(readlink /proc/$$/fd/1); e=$(readlink /proc/$$/fd/2)\n\
             f=$(mktemp '{r}/run.XXXXXX')\n\
             : > \"$f.args\"\n\
             [ $# -gt 0 ] && printf '%s\\0' \"$@\" > \"$f.args\"\n\
             cp /proc/$$/environ \"$f.env\"\n\
             cp /proc/$$/stat \"$f.stat\"\n\
             readlink /proc/$$/cwd > \"$f.cwd\"\n\
             printf '%s\\n%s\\n%s\\n' \"$i\" \"$o\" \"$e\" > \"$f.fds\"\n\
             {mark}\
             echo \"$f\" >> '{r}/order'\n\
             {then}\n"
        ),
    );
}

impl Recording {
    /// Put a stand-in for `name` on this thread too, its runs written down
    /// with this recording's and in the one order they all started in, each
    /// under its own name: for programs that work together, as ssh, rsync
    /// and scp do.
    pub(crate) fn also(&mut self, dir: &Path, name: &str, then: &str) {
        let mark = format!("printf %s '{name}' > \"$f.name\"\n");
        recording_stand_in(dir, name, &self.runs, &mark, then);
    }

    /// The runs since the last call, two lines each: the command, then where
    /// it ran, what croft changed in its environment, its three streams, and
    /// whether it runs apart from croft's session. Each path in `names` is
    /// written as its name, longest first.
    pub(crate) fn take(&mut self, names: &[(&Path, &str)]) -> Vec<String> {
        let order = std::fs::read_to_string(self.runs.join("order")).unwrap_or_default();
        let runs: Vec<&str> = order.lines().skip(self.taken).collect();
        self.taken += runs.len();
        let mut names: Vec<(String, &str)> = names
            .iter()
            .map(|(path, name)| (path.display().to_string(), *name))
            .collect();
        names.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
        let scratch = format!("{}/", std::env::temp_dir().display());
        let named = |text: &str| {
            let text = names.iter().fold(text.to_string(), |text, (path, name)| {
                text.replace(path, name)
            });
            scratch_named(&text, &scratch)
        };
        let env_now: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
        runs.iter()
            .map(|run| self.render(Path::new(run), &named, &env_now))
            .collect()
    }

    fn render(
        &self,
        run: &Path,
        named: &dyn Fn(&str) -> String,
        env_now: &BTreeMap<OsString, OsString>,
    ) -> String {
        let read = |ext: &str| std::fs::read(format!("{}.{ext}", run.display())).unwrap();
        let args: Vec<String> = read("args")
            .split(|b| *b == 0)
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        // `printf '%s\0'` ends every argument with a NUL, so the last piece is empty.
        let args = &args[..args.len().saturating_sub(1)];
        // A program put in place with `also` names itself.
        let mut line = std::fs::read_to_string(format!("{}.name", run.display()))
            .unwrap_or_else(|_| self.name.clone());
        for arg in args {
            line.push(' ');
            line.push_str(&quoted(&named(arg)));
        }

        let env: BTreeMap<OsString, OsString> = read("env")
            .split(|b| *b == 0)
            .filter(|kv| !kv.is_empty())
            .map(|kv| {
                let kv = String::from_utf8_lossy(kv);
                let (k, v) = kv.split_once('=').unwrap_or((&kv, ""));
                (OsString::from(k), OsString::from(v))
            })
            .collect();
        let steady = |k: &OsString| self.env_before.get(k) == env_now.get(k);
        let mut changes: Vec<String> = env
            .iter()
            .filter(|(k, v)| steady(k) && self.env_before.get(*k) != Some(*v))
            .map(|(k, v)| named(&format!("{}={}", k.to_string_lossy(), v.to_string_lossy())))
            .collect();
        changes.extend(
            self.env_before
                .keys()
                .filter(|k| steady(k) && !env.contains_key(*k))
                .map(|k| format!("-{}", k.to_string_lossy())),
        );
        let env = if changes.is_empty() {
            String::from("unchanged")
        } else {
            changes.join(" ")
        };

        let cwd = String::from_utf8_lossy(&read("cwd")).trim_end().to_string();
        let ours = std::env::current_dir().unwrap();
        let cwd = if Path::new(&cwd) == ours {
            String::from("croft's folder")
        } else {
            named(&cwd)
        };

        let fds = String::from_utf8_lossy(&read("fds")).into_owned();
        let stream = |target: &str| -> String {
            if target == "/dev/null" {
                String::from("nowhere")
            } else if target.starts_with("pipe:[") {
                String::from("pipe")
            } else if target.starts_with("socket:[") {
                String::from("socket")
            } else if target.starts_with("/dev/pts/") || target.starts_with("/dev/tty") {
                String::from("terminal")
            } else if target.is_empty() {
                String::from("closed")
            } else {
                named(target)
            }
        };
        let mut fds = fds.lines().map(stream);
        let (stdin, stdout, stderr) = (
            fds.next().unwrap_or_default(),
            fds.next().unwrap_or_default(),
            fds.next().unwrap_or_default(),
        );

        // `pid (comm) state ppid pgrp session …`; comm may hold spaces.
        let stat = String::from_utf8_lossy(&read("stat")).into_owned();
        let pid = stat.split(' ').next().unwrap_or_default();
        let after = stat.rsplit_once(") ").map(|(_, a)| a).unwrap_or_default();
        let fields: Vec<&str> = after.split(' ').collect();
        let (pgrp, session) = (fields.get(2).copied(), fields.get(3).copied());
        let apart = if session == Some(pid) {
            "own session"
        } else if pgrp == Some(pid) {
            "own process group"
        } else {
            "croft's session"
        };

        format!(
            "{line}\n    in {cwd} | env {env} | stdin {stdin} | stdout {stdout} | stderr {stderr} | {apart}"
        )
    }
}

/// `text` with each folder croft made for itself in the temp directory
/// `scratch` written `<scratch>`: its name changes from run to run. Run after
/// the caller's own names, so a folder the test named is not one of these.
fn scratch_named(text: &str, scratch: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(scratch) {
        out.push_str(&rest[..at]);
        out.push_str("<scratch>");
        let after = &rest[at + scratch.len()..];
        let end = after.find(['/', ' ', '\'']).unwrap_or(after.len());
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// `arg` as a shell would need it written: bare when it is plain, else in
/// single quotes.
fn quoted(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,^~<>{}".contains(c));
    if plain {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// The real `name` on `PATH`, for a recording stand-in to run after it has
/// written a run down.
pub(crate) fn real_program(name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(name))
                .find(|p| {
                    std::fs::metadata(p)
                        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                })
        })
        .unwrap_or_else(|| panic!("{name} is on PATH"))
}

/// Compare `now` with the saved record at `saved`, failing with the lines
/// that differ; with `CROFT_BLESS=1`, write `now` there instead.
pub(crate) fn check_saved(saved: &Path, now: &str) {
    if std::env::var_os("CROFT_BLESS").is_some_and(|v| v == "1") {
        std::fs::write(saved, now).unwrap();
        return;
    }
    let before = match std::fs::read_to_string(saved) {
        Ok(before) => before,
        // A published crate's source leaves the saved records out, as it
        // does the screen goldens.
        Err(_) if packaged_source() => return,
        Err(err) => panic!(
            "no saved record at {} ({err}): write it with CROFT_BLESS=1",
            saved.display()
        ),
    };
    if before == now {
        return;
    }
    let (old, new): (Vec<&str>, Vec<&str>) = (before.lines().collect(), now.lines().collect());
    let first = old
        .iter()
        .zip(&new)
        .position(|(a, b)| a != b)
        .unwrap_or(old.len().min(new.len()));
    let from = first.saturating_sub(3);
    let shown = |lines: &[&str], sign: &str| {
        lines
            .iter()
            .enumerate()
            .skip(from)
            .take(first - from + 6)
            .map(|(i, l)| format!("{sign}{:>5} {l}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    };
    panic!(
        "the recorded calls differ from {} from line {}. If the change is meant, \
         rewrite it with CROFT_BLESS=1.\n--- saved\n{}\n--- now\n{}",
        saved.display(),
        first + 1,
        shown(&old, "-"),
        shown(&new, "+"),
    );
}

/// Whether this is a published crate's source, which leaves saved records
/// out: `cargo package` keeps the manifest it was given under this name
/// beside the one it publishes, and a checkout never has it.
fn packaged_source() -> bool {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("Cargo.toml.orig")
        .is_file()
}
