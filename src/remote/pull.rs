//! The relay's pull request: which remote paths may be pulled, and copying them to this
//! machine.

use super::*;

/// Whether a pull may send `src`. The request comes from the remote's
/// relay log, which any process running as the remote user can append to,
/// so it is not proof the user dropped that file. A drag from Finder names
/// a visible file; credentials live under hidden paths (`~/.ssh`,
/// `~/.aws`, `~/.gnupg`, `~/.netrc`, `~/.config/...`). So a path that is
/// not absolute, or that resolves (through `..` or symlinks) to anything
/// under a hidden component, is refused.
pub(super) fn pull_source_allowed(src: &Path) -> bool {
    use std::path::Component;
    let hidden = |p: &Path| {
        p.components().any(|c| match c {
            Component::Normal(name) => name.to_string_lossy().starts_with('.'),
            Component::ParentDir => true,
            _ => false,
        })
    };
    if !src.is_absolute() || hidden(src) {
        return false;
    }
    std::fs::canonicalize(src).is_ok_and(|real| !hidden(&real))
}

/// The local `tar` arguments that pack a pulled `basename` from `parent`.
/// Hidden entries are excluded: [`pull_source_allowed`] vets only the named
/// path, and a pulled folder (`~`, say) would otherwise carry its `.ssh`
/// along. `--` keeps a name starting with `-` from reading as an option.
/// Both GNU tar and bsdtar match `.*` against each path component.
pub(super) fn pull_tar_args(parent: &Path, basename: &std::ffi::OsStr) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = ["-c", "-f", "-", "--exclude", ".*", "-C"]
        .iter()
        .map(Into::into)
        .collect();
    args.push(parent.into());
    args.push("--".into());
    args.push(basename.into());
    args
}

pub(super) fn handle_pull_request(
    host: &str,
    socket: &Path,
    inbox_dir: &str,
    request_id: &str,
    src: &str,
) {
    let src_path = PathBuf::from(src);
    let dest_dir = format!("{inbox_dir}/{request_id}");
    if src_path.exists() && !pull_source_allowed(&src_path) {
        write_relay_err(
            host,
            socket,
            inbox_dir,
            request_id,
            &format!("refusing to send a hidden path: {}", src_path.display()),
        );
        return;
    }
    if !src_path.exists() {
        write_relay_err(
            host,
            socket,
            inbox_dir,
            request_id,
            &format!("local file not found: {}", src_path.display()),
        );
        return;
    }
    let Some(parent) = src_path.parent() else {
        write_relay_err(host, socket, inbox_dir, request_id, "source has no parent");
        return;
    };
    let Some(basename) = src_path.file_name() else {
        write_relay_err(
            host,
            socket,
            inbox_dir,
            request_id,
            "source has no basename",
        );
        return;
    };
    // Pipe a tar of the source through ssh into a remote tar -x. This
    // copies file or directory equivalently in one round trip and
    // sidesteps OpenSSH 9.x scp/sftp's realpath check on the
    // destination, which kept failing on freshly-mkdir'd dirs.
    let mut tar = match crate::proc::command("tar")
        .env("COPYFILE_DISABLE", "1")
        .args(pull_tar_args(parent, basename))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            write_relay_err(
                host,
                socket,
                inbox_dir,
                request_id,
                &format!("local tar spawn failed: {e}"),
            );
            return;
        }
    };
    // Read while both run: a tar blocked writing a full stderr pipe never
    // finishes its stdout, so ssh never sees the end of its input and the
    // relay waits on it for good (#2795).
    let tar_stderr = tar.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = std::io::Read::read_to_end(&mut err, &mut bytes);
            bytes
        })
    });
    let tar_stdout = match tar.stdout.take() {
        Some(s) => s,
        None => {
            let _ = tar.wait();
            write_relay_err(host, socket, inbox_dir, request_id, "tar stdout missing");
            return;
        }
    };
    let remote_cmd = format!(
        "set -e; mkdir -p {dir} && tar -x -f - -C {dir}",
        dir = shell_quote(&dest_dir),
    );
    let ssh_recv = match crate::proc::command("ssh")
        .arg("-S")
        .arg(socket)
        .arg("-o")
        .arg("ControlMaster=no")
        .arg(host)
        .arg(&remote_cmd)
        .stdin(Stdio::from(tar_stdout))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tar.wait();
            write_relay_err(
                host,
                socket,
                inbox_dir,
                request_id,
                &format!("ssh recv spawn failed: {e}"),
            );
            return;
        }
    };
    let ssh_out = ssh_recv.wait_with_output();
    let tar_out = tar.wait().map(|status| std::process::Output {
        status,
        stdout: Vec::new(),
        stderr: tar_stderr
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default(),
    });
    match (tar_out, ssh_out) {
        (Ok(t), Ok(s)) if t.status.success() && s.status.success() => {
            let touch = format!("touch {}/.ok", shell_quote(&dest_dir));
            let _ = crate::proc::command("ssh")
                .arg("-S")
                .arg(socket)
                .arg("-o")
                .arg("ControlMaster=no")
                .arg(host)
                .arg(&touch)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        (Ok(t), Ok(s)) => {
            let mut detail = format!("tar exited {}, ssh exited {}", t.status, s.status);
            let tar_err = String::from_utf8_lossy(&t.stderr);
            let ssh_err = String::from_utf8_lossy(&s.stderr);
            if !tar_err.trim().is_empty() {
                detail.push_str(" | tar: ");
                detail.push_str(tar_err.trim());
            }
            if !ssh_err.trim().is_empty() {
                detail.push_str(" | remote: ");
                detail.push_str(ssh_err.trim());
            }
            write_relay_err(host, socket, inbox_dir, request_id, &detail);
        }
        (Err(e), _) | (_, Err(e)) => write_relay_err(
            host,
            socket,
            inbox_dir,
            request_id,
            &format!("relay pipe wait failed: {e}"),
        ),
    }
}
