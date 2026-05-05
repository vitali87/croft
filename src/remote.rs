use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::hash::Hasher;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteTarget {
    pub alias: String,
    pub host_name: Option<String>,
    pub user: Option<String>,
}

impl RemoteTarget {
    pub fn detail(&self) -> String {
        match (&self.user, &self.host_name) {
            (Some(user), Some(host)) => format!("{user}@{host}"),
            (Some(user), None) => format!("{user}@{}", self.alias),
            (None, Some(host)) if host != &self.alias => host.clone(),
            _ => String::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SshConfigState {
    entries: Vec<SshConfigEntryState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SshConfigEntryState {
    path: PathBuf,
    modified: Option<std::time::SystemTime>,
    len: Option<u64>,
}

pub fn ssh_config_state() -> SshConfigState {
    SshConfigState {
        entries: ssh_config_paths()
            .into_iter()
            .map(|path| {
                let meta = std::fs::metadata(&path).ok();
                SshConfigEntryState {
                    path,
                    modified: meta.as_ref().and_then(|m| m.modified().ok()),
                    len: meta.map(|m| m.len()),
                }
            })
            .collect(),
    }
}

#[derive(Default)]
struct HostBlock {
    aliases: Vec<String>,
    host_name: Option<String>,
    user: Option<String>,
}

pub fn discover_ssh_targets() -> Vec<RemoteTarget> {
    ssh_config_paths()
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .flat_map(|content| parse_ssh_config(&content))
        .fold(
            (BTreeSet::new(), Vec::new()),
            |(mut seen, mut targets), target| {
                if seen.insert(target.alias.clone()) {
                    targets.push(target);
                }
                (seen, targets)
            },
        )
        .1
}

fn ssh_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".ssh/config"));
    }
    paths
}

pub fn parse_ssh_config(input: &str) -> Vec<RemoteTarget> {
    let mut targets = Vec::new();
    let mut block = HostBlock::default();

    for raw in input.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else {
            continue;
        };
        let key = key.to_ascii_lowercase();
        if key == "host" {
            flush_block(&mut targets, &mut block);
            block.aliases = parts
                .filter(|alias| is_explicit_host_alias(alias))
                .map(ToString::to_string)
                .collect();
            continue;
        }
        if block.aliases.is_empty() {
            continue;
        }
        match key.as_str() {
            "hostname" => block.host_name = parts.next().map(ToString::to_string),
            "user" => block.user = parts.next().map(ToString::to_string),
            _ => {}
        }
    }
    flush_block(&mut targets, &mut block);
    targets
}

fn flush_block(targets: &mut Vec<RemoteTarget>, block: &mut HostBlock) {
    for alias in block.aliases.drain(..) {
        targets.push(RemoteTarget {
            alias,
            host_name: block.host_name.clone(),
            user: block.user.clone(),
        });
    }
    block.host_name = None;
    block.user = None;
}

fn strip_comment(line: &str) -> &str {
    line.split_once('#').map(|(head, _)| head).unwrap_or(line)
}

fn is_explicit_host_alias(alias: &str) -> bool {
    !alias.starts_with('!') && !alias.contains('*') && !alias.contains('?')
}

pub fn launch_croft(host: &str, path: Option<&str>) -> Result<()> {
    println!("Connecting to {host}...");
    let ssh = SshControl::start(host)?;
    let local_stamp = local_source_stamp()?;
    if remote_install_needed(&ssh, &local_stamp)? {
        println!("Installing/updating Croft on {host}...");
        install_remote_croft(&ssh, &local_stamp)?;
    }
    let status = run_remote_croft(&ssh, path)?;
    if status.success() {
        return Ok(());
    }
    if status.code() == Some(127) {
        println!("Croft is not installed on {host}; bootstrapping from local source...");
        install_remote_croft(&ssh, &local_stamp)?;
        println!("Reconnecting to {host}...");
        let status = run_remote_croft(&ssh, path)?;
        if status.success() {
            return Ok(());
        }
        anyhow::bail!("ssh exited with {status}");
    }
    anyhow::bail!("ssh exited with {status}");
}

struct SshControl {
    host: String,
    socket_dir: PathBuf,
    socket_path: PathBuf,
}

impl SshControl {
    fn start(host: &str) -> Result<Self> {
        let socket_dir = ssh_control_dir()?;
        std::fs::create_dir_all(&socket_dir)
            .with_context(|| format!("creating {}", socket_dir.display()))?;
        let socket_path = socket_dir.join("ctl");
        let status = Command::new("ssh")
            .arg("-M")
            .arg("-S")
            .arg(&socket_path)
            .arg("-f")
            .arg("-N")
            .arg("-T")
            .arg(host)
            .status()
            .context("starting SSH control connection")?;
        if !status.success() {
            anyhow::bail!("ssh control connection exited with {status}");
        }
        Ok(Self {
            host: host.to_string(),
            socket_dir,
            socket_path,
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new("ssh");
        command
            .arg("-S")
            .arg(&self.socket_path)
            .arg("-o")
            .arg("ControlMaster=no");
        command
    }
}

impl Drop for SshControl {
    fn drop(&mut self) {
        let _ = Command::new("ssh")
            .arg("-S")
            .arg(&self.socket_path)
            .arg("-O")
            .arg("exit")
            .arg(&self.host)
            .status();
        let _ = std::fs::remove_dir_all(&self.socket_dir);
    }
}

fn ssh_control_dir() -> Result<PathBuf> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_millis();
    Ok(std::env::temp_dir().join(format!(
        "croft-ssh-{}-{now}",
        std::process::id()
    )))
}

fn run_remote_croft(ssh: &SshControl, path: Option<&str>) -> Result<ExitStatus> {
    let mut command = ssh.command();
    command
        .arg("-tt")
        .arg(&ssh.host)
        .arg(remote_croft_command(path));
    command.status().context("starting ssh")
}

fn install_remote_croft(ssh: &SshControl, source_stamp: &str) -> Result<()> {
    sync_local_source_to_remote(ssh)?;
    let status = ssh
        .command()
        .arg(&ssh.host)
        .arg(remote_install_command(source_stamp))
        .status()
        .context("installing croft on remote")?;
    if !status.success() {
        anyhow::bail!("remote croft install failed with {status}");
    }
    Ok(())
}

pub fn remote_croft_command(path: Option<&str>) -> String {
    remote_croft_command_for_terminal(
        path,
        std::env::var("TERM_PROGRAM").ok().as_deref(),
    )
}

fn remote_croft_command_for_terminal(path: Option<&str>, term_program: Option<&str>) -> String {
    let mut prefix = String::new();
    if let Some(term_program) =
        term_program.filter(|value| crate::iterm2_inline::is_iterm2_term_program(Some(value)))
    {
        prefix.push_str("export CROFT_FORCE_INLINE_IMAGES=1 TERM_PROGRAM=");
        prefix.push_str(&shell_quote(term_program));
        prefix.push_str("; ");
    }
    prefix.push_str("export PATH=\"$HOME/.cargo/bin:$PATH\"; command -v croft >/dev/null 2>&1 || { echo 'croft not found on remote PATH' >&2; exit 127; }; exec croft");
    match path.filter(|p| !p.is_empty()) {
        Some(path) => format!("{prefix} {}", shell_quote(path)),
        None => prefix,
    }
}

fn remote_install_needed(ssh: &SshControl, local_stamp: &str) -> Result<bool> {
    let output = ssh
        .command()
        .arg(&ssh.host)
        .arg(remote_install_check_command())
        .output()
        .context("checking remote croft install")?;
    if !output.status.success() {
        return Ok(true);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim() != local_stamp)
}

fn remote_install_check_command() -> &'static str {
    r#"if [ -f "$HOME/.cargo/env" ]; then
  . "$HOME/.cargo/env"
fi
export PATH="$HOME/.cargo/bin:$PATH"
command -v croft >/dev/null 2>&1 && cat "$HOME/.cache/croft/install-stamp" 2>/dev/null"#
}

fn sync_local_source_to_remote(ssh: &SshControl) -> Result<()> {
    match sync_via_rsync(ssh) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Fallback path so first-time installs on minimal remotes
            // (alpine images, locked-down VMs) that ship without rsync
            // still succeed. The trade-off is a from-scratch dependency
            // rebuild on those hosts, same as the legacy behaviour.
            eprintln!("rsync sync failed ({e}); falling back to tar pipe");
            sync_via_tar(ssh)
        }
    }
}

/// Mirror the local source tree onto the remote with rsync, preserving
/// mtimes and skipping unchanged files. Critically, this does NOT delete
/// `target/` on the remote, so `cargo install` does an incremental rebuild
/// (typically rebuilding only the 1-3 crates that actually changed)
/// instead of starting from scratch every time.
fn sync_via_rsync(ssh: &SshControl) -> Result<()> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mkdir_status = ssh
        .command()
        .arg(&ssh.host)
        .arg("mkdir -p \"$HOME/.cache/croft/source\"")
        .status()
        .context("creating remote source dir")?;
    if !mkdir_status.success() {
        anyhow::bail!("remote mkdir exited with {mkdir_status}");
    }
    let ssh_e = format!(
        "ssh -S {} -o ControlMaster=no",
        shell_quote_for_e_arg(&ssh.socket_path),
    );
    let mut source_arg: std::ffi::OsString = source.clone().into_os_string();
    source_arg.push("/");
    let dest = format!("{}:.cache/croft/source/", ssh.host);
    let status = Command::new("rsync")
        .args([
            "-a",
            "-z",
            "--delete",
            "--exclude=target",
            "--exclude=.git",
            "--exclude=.DS_Store",
            "--exclude=assets/.DS_Store",
            "-e",
        ])
        .arg(&ssh_e)
        .arg(&source_arg)
        .arg(&dest)
        .status()
        .context("running rsync to remote")?;
    if !status.success() {
        anyhow::bail!("rsync exited with {status}");
    }
    Ok(())
}

/// Last-resort tar pipe sync used when rsync isn't available on either
/// side. This path also keeps the remote source dir in place rather than
/// blowing it away — extracting over an existing tree overwrites changed
/// files, so `target/` still survives between installs even when the user
/// is forced down this branch.
fn sync_via_tar(ssh: &SshControl) -> Result<()> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut tar = Command::new("tar")
        .env("COPYFILE_DISABLE", "1")
        .args([
            "-czf",
            "-",
            "--exclude=.git",
            "--exclude=target",
            "--exclude=.DS_Store",
            "--exclude=assets/.DS_Store",
            "-C",
        ])
        .arg(&source)
        .arg(".")
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("packing {}", source.display()))?;

    let tar_stdout = tar.stdout.take().context("opening tar stdout")?;
    let mut remote = ssh
        .command()
        .arg(&ssh.host)
        .arg(
            "mkdir -p \"$HOME/.cache/croft/source\" && \
             tar -xzf - -C \"$HOME/.cache/croft/source\"",
        )
        .stdin(Stdio::from(tar_stdout))
        .spawn()
        .context("copying croft source to remote")?;

    let ssh_status = remote.wait().context("waiting for remote source copy")?;
    let tar_status = tar.wait().context("waiting for local source archive")?;
    if !tar_status.success() {
        anyhow::bail!("local source archive failed with {tar_status}");
    }
    if !ssh_status.success() {
        anyhow::bail!("remote source copy failed with {ssh_status}");
    }
    Ok(())
}

/// Quote a path for embedding inside rsync's `-e "ssh -S <path> ..."`
/// argument. rsync executes the remote-shell string through `/bin/sh -c`,
/// so any spaces or shell metacharacters in the control-socket path would
/// break the invocation. The control socket lives under
/// `std::env::temp_dir()` which is normally space-free, but the socket
/// path is process-id + millisecond-stamped so quote defensively rather
/// than trust the format.
fn shell_quote_for_e_arg(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    if s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-')) {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn remote_install_command(source_stamp: &str) -> String {
    format!(
        r#"set -e
if [ -f "$HOME/.cargo/env" ]; then
  . "$HOME/.cargo/env"
fi
export PATH="$HOME/.cargo/bin:$PATH"
if ! command -v cargo >/dev/null 2>&1; then
  if ! command -v curl >/dev/null 2>&1; then
    if command -v apt-get >/dev/null 2>&1 && [ "$(id -u)" = "0" ]; then
      export DEBIAN_FRONTEND=noninteractive
      apt-get update
      apt-get install -y curl ca-certificates build-essential pkg-config
    else
      echo 'cargo and curl are missing; install Rust/Cargo on the remote and retry' >&2
      exit 127
    fi
  fi
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  . "$HOME/.cargo/env"
fi
export PATH="$HOME/.cargo/bin:$PATH"
cargo install --path "$HOME/.cache/croft/source" --force --locked
mkdir -p "$HOME/.cache/croft"
printf %s {stamp} > "$HOME/.cache/croft/install-stamp"
"#,
        stamp = shell_quote(source_stamp)
    )
}

fn local_source_stamp() -> Result<String> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hash_source_dir(&source, &source, &mut hasher)?;
    Ok(format!("{:016x}", hasher.finish()))
}

fn hash_source_dir(root: &PathBuf, dir: &PathBuf, hasher: &mut impl Hasher) -> Result<()> {
    let mut entries = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("reading entries in {}", dir.display()))?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if matches!(name.as_ref(), ".git" | "target" | ".DS_Store") {
            continue;
        }
        let meta = entry
            .metadata()
            .with_context(|| format!("reading metadata for {}", path.display()))?;
        let rel = path.strip_prefix(root).unwrap_or(&path);
        hasher.write(rel.to_string_lossy().as_bytes());
        if meta.is_dir() {
            hasher.write_u8(b'/');
            hash_source_dir(root, &path, hasher)?;
        } else if meta.is_file() {
            hasher.write_u64(meta.len());
            hasher.write(&std::fs::read(&path).with_context(|| {
                format!("reading {}", path.display())
            })?);
        }
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
fn ssh_control_socket_path_for_test(dir: &Path) -> PathBuf {
    dir.join("ctl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ssh_config_extracts_explicit_hosts() {
        let targets = parse_ssh_config(
            r#"
Host *
  User nobody

Host genesis-cloud github.com
  HostName 10.0.0.2
  User ubuntu

Host !blocked *.internal
  User root
"#,
        );
        assert_eq!(
            targets,
            vec![
                RemoteTarget {
                    alias: String::from("genesis-cloud"),
                    host_name: Some(String::from("10.0.0.2")),
                    user: Some(String::from("ubuntu")),
                },
                RemoteTarget {
                    alias: String::from("github.com"),
                    host_name: Some(String::from("10.0.0.2")),
                    user: Some(String::from("ubuntu")),
                },
            ]
        );
    }

    #[test]
    fn remote_croft_command_quotes_paths() {
        assert_eq!(
            remote_croft_command_for_terminal(Some("/tmp/it's here"), None),
            "export PATH=\"$HOME/.cargo/bin:$PATH\"; command -v croft >/dev/null 2>&1 || { echo 'croft not found on remote PATH' >&2; exit 127; }; exec croft '/tmp/it'\"'\"'s here'"
        );
    }

    #[test]
    fn remote_croft_command_forwards_supported_terminal_program() {
        assert_eq!(
            remote_croft_command_for_terminal(None, Some("iTerm.app")),
            "export CROFT_FORCE_INLINE_IMAGES=1 TERM_PROGRAM='iTerm.app'; export PATH=\"$HOME/.cargo/bin:$PATH\"; command -v croft >/dev/null 2>&1 || { echo 'croft not found on remote PATH' >&2; exit 127; }; exec croft"
        );
    }

    #[test]
    fn remote_install_command_installs_from_staged_source() {
        let command = remote_install_command("abc123");
        assert!(command.contains("cargo install --path \"$HOME/.cache/croft/source\""));
        assert!(command.contains("rustup.rs"));
        assert!(command.contains("printf %s 'abc123' > \"$HOME/.cache/croft/install-stamp\""));
    }

    #[test]
    fn remote_install_check_uses_cargo_path_before_probe() {
        let command = remote_install_check_command();
        let path_pos = command.find("export PATH=\"$HOME/.cargo/bin:$PATH\"").unwrap();
        let probe_pos = command.find("command -v croft").unwrap();
        assert!(command.contains(". \"$HOME/.cargo/env\""));
        assert!(path_pos < probe_pos);
        assert!(command.contains("cat \"$HOME/.cache/croft/install-stamp\""));
    }

    #[test]
    fn shell_quote_for_e_arg_passes_alnum_paths_through() {
        assert_eq!(
            shell_quote_for_e_arg(Path::new("/tmp/croft-ssh-1234/ctl")),
            "/tmp/croft-ssh-1234/ctl",
        );
    }

    #[test]
    fn shell_quote_for_e_arg_quotes_paths_with_spaces() {
        let q = shell_quote_for_e_arg(Path::new("/Users/v a/croft/ctl"));
        assert_eq!(q, "'/Users/v a/croft/ctl'");
    }

    #[test]
    fn shell_quote_for_e_arg_escapes_single_quotes_inside_path() {
        let q = shell_quote_for_e_arg(Path::new("/tmp/it's/ctl"));
        // /tmp/it's → /tmp/it'\''s after the standard sh single-quote
        // escaping trick (close, escaped quote, reopen).
        assert_eq!(q, "'/tmp/it'\\''s/ctl'");
    }

    #[test]
    fn ssh_control_socket_path_is_short_and_inside_temp_dir() {
        let dir = std::env::temp_dir().join("croft-ssh-test");
        let socket = ssh_control_socket_path_for_test(&dir);
        assert!(socket.starts_with(&dir));
        assert_eq!(socket.file_name().and_then(|s| s.to_str()), Some("ctl"));
    }
}
