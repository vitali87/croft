//! Dev Containers (#617): `croft devcontainer [path]`.
//!
//! Reads the workspace's `.devcontainer/devcontainer.json` (or
//! `.devcontainer.json`), builds or pulls its image, starts a container with
//! the workspace mounted, runs `postCreateCommand` once, installs a croft
//! binary into it the way `croft <host>` provisions a remote (a static musl
//! cross-build when the toolchain is there), then runs croft inside it on
//! this terminal.
//!
//! The container is labelled with the workspace path and a hash of the
//! config, so the next run reattaches to it (starting it if stopped) instead
//! of creating another, and an edited config gets a fresh one.
//!
//! Supported keys: `image`, `build` (`dockerfile`, `context`, `args`),
//! `mounts`, `forwardPorts`, `postCreateCommand`, plus `workspaceFolder`,
//! `workspaceMount`, `containerEnv`, `remoteUser` and `runArgs`. Features and
//! Compose are not.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where the binary goes inside the container.
const CONTAINER_BINARY: &str = "/usr/local/bin/croft";
/// Label naming the workspace a container belongs to.
const WORKSPACE_LABEL: &str = "croft.devcontainer";
/// Label holding the hash of the config the container was made from.
const CONFIG_LABEL: &str = "croft.devcontainer.config";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Image(String),
    Build {
        dockerfile: PathBuf,
        context: PathBuf,
        args: Vec<(String, String)>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevContainer {
    pub source: Source,
    /// `--mount` values, variables already substituted.
    pub mounts: Vec<String>,
    /// `-p` values.
    pub ports: Vec<String>,
    /// Commands run once after the container is created, each an argv.
    pub post_create: Vec<Vec<String>>,
    pub workspace_folder: String,
    pub workspace_mount: String,
    pub env: Vec<(String, String)>,
    pub remote_user: Option<String>,
    pub run_args: Vec<String>,
}

/// The config file for workspace `root`, if it has one.
pub fn find_config(root: &Path) -> Option<PathBuf> {
    [
        root.join(".devcontainer").join("devcontainer.json"),
        root.join(".devcontainer.json"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// Expand `${localWorkspaceFolder}`, `${localWorkspaceFolderBasename}`,
/// `${containerWorkspaceFolder}` and `${localEnv:NAME}`.
fn substitute(
    s: &str,
    root: &Path,
    container_ws: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let Some(len) = rest[start..].find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let var = &rest[start + 2..start + len];
        let value = match var {
            "localWorkspaceFolder" => Some(root.display().to_string()),
            "localWorkspaceFolderBasename" => Some(basename(root)),
            "containerWorkspaceFolder" => Some(container_ws.to_string()),
            _ => var.strip_prefix("localEnv:").map(|name| {
                let (name, default) = name.split_once(':').unwrap_or((name, ""));
                env(name).unwrap_or_else(|| default.to_string())
            }),
        };
        match value {
            Some(v) => out.push_str(&v),
            None => out.push_str(&rest[start..start + len + 1]),
        }
        rest = &rest[start + len + 1..];
    }
    out.push_str(rest);
    out
}

fn basename(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("workspace"))
}

/// A lifecycle command as argvs: a string runs through `sh -c`, an array is
/// an argv, an object is several of either, run in key order.
fn commands(v: &Value) -> Vec<Vec<String>> {
    match v {
        Value::String(s) if !s.trim().is_empty() => {
            vec![vec!["sh".into(), "-c".into(), s.clone()]]
        }
        Value::Array(a) => {
            let argv: Vec<String> = a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect();
            if argv.is_empty() { vec![] } else { vec![argv] }
        }
        Value::Object(m) => m.values().flat_map(commands).collect(),
        _ => vec![],
    }
}

/// Parse `text` (JSONC) read from `config` for workspace `root`.
pub fn parse(
    text: &str,
    config: &Path,
    root: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<DevContainer> {
    let v: Value = serde_json::from_str(&crate::tasks::strip_jsonc(text))
        .with_context(|| format!("parsing {}", config.display()))?;
    let dir = config.parent().unwrap_or(root);
    let str_of = |key: &str| v.get(key).and_then(Value::as_str);

    let workspace_folder = str_of("workspaceFolder")
        .map(|s| substitute(s, root, "", env))
        .unwrap_or_else(|| format!("/workspaces/{}", basename(root)));
    let sub = |s: &str| substitute(s, root, &workspace_folder, env);

    let build = v.get("build");
    let dockerfile = build
        .and_then(|b| b.get("dockerfile"))
        .and_then(Value::as_str)
        .or_else(|| str_of("dockerFile"));
    let source = if let Some(dockerfile) = dockerfile {
        let context = build
            .and_then(|b| b.get("context"))
            .and_then(Value::as_str)
            .unwrap_or(".");
        let args = build
            .and_then(|b| b.get("args"))
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), sub(v))))
                    .collect()
            })
            .unwrap_or_default();
        Source::Build {
            dockerfile: dir.join(dockerfile),
            context: dir.join(context),
            args,
        }
    } else if let Some(image) = str_of("image") {
        Source::Image(sub(image))
    } else {
        bail!(
            "{} names neither `image` nor `build.dockerfile`",
            config.display()
        );
    };

    let mounts = v
        .get("mounts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| match m {
            Value::String(s) => Some(sub(s)),
            Value::Object(o) => {
                let field = |k: &str| o.get(k).and_then(Value::as_str).map(sub);
                let target = field("target")?;
                let kind = field("type").unwrap_or_else(|| String::from("bind"));
                Some(match field("source") {
                    Some(source) => format!("type={kind},source={source},target={target}"),
                    None => format!("type={kind},target={target}"),
                })
            }
            _ => None,
        })
        .collect();

    // A port that is a number (or a numeric string) is published on the
    // loopback interface; `"service:port"` entries name Compose services,
    // which croft doesn't run.
    let ports = v
        .get("forwardPorts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| match p {
            Value::Number(n) => n.as_u64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        })
        .filter(|p| (1..=65535).contains(p))
        .map(|p| format!("127.0.0.1:{p}:{p}"))
        .collect();

    let workspace_mount = str_of("workspaceMount").map(sub).unwrap_or_else(|| {
        format!(
            "type=bind,source={},target={workspace_folder}",
            root.display()
        )
    });

    Ok(DevContainer {
        source,
        mounts,
        ports,
        post_create: v.get("postCreateCommand").map(commands).unwrap_or_default(),
        env: v
            .get("containerEnv")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), sub(v))))
                    .collect()
            })
            .unwrap_or_default(),
        remote_user: str_of("remoteUser").map(str::to_string),
        run_args: v
            .get("runArgs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str().map(sub))
            .collect(),
        workspace_folder,
        workspace_mount,
    })
}

/// FNV-1a, hex: a stable short id for a path or a config.
fn hash(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// The tag a `build` config's image gets.
fn image_tag(root: &Path) -> String {
    format!(
        "croft-devcontainer-{}",
        &hash(&root.display().to_string())[..12]
    )
}

/// `docker build` arguments for a `build` config, `None` for an `image`.
pub fn build_args(dc: &DevContainer, root: &Path) -> Option<Vec<String>> {
    let Source::Build {
        dockerfile,
        context,
        args,
    } = &dc.source
    else {
        return None;
    };
    let mut out = vec![
        "build".to_string(),
        "-t".into(),
        image_tag(root),
        "-f".into(),
        dockerfile.display().to_string(),
    ];
    for (k, v) in args {
        out.push("--build-arg".into());
        out.push(format!("{k}={v}"));
    }
    out.push(context.display().to_string());
    Some(out)
}

/// `docker run` arguments that create the container.
pub fn run_args(dc: &DevContainer, root: &Path, config_hash: &str) -> Vec<String> {
    let image = match &dc.source {
        Source::Image(i) => i.clone(),
        Source::Build { .. } => image_tag(root),
    };
    let mut out = vec![
        "run".to_string(),
        "-d".into(),
        "--init".into(),
        "--label".into(),
        format!("{WORKSPACE_LABEL}={}", root.display()),
        "--label".into(),
        format!("{CONFIG_LABEL}={config_hash}"),
        "--mount".into(),
        dc.workspace_mount.clone(),
        "-w".into(),
        dc.workspace_folder.clone(),
    ];
    for m in &dc.mounts {
        out.push("--mount".into());
        out.push(m.clone());
    }
    for p in &dc.ports {
        out.push("-p".into());
        out.push(p.clone());
    }
    for (k, v) in &dc.env {
        out.push("-e".into());
        out.push(format!("{k}={v}"));
    }
    out.extend(dc.run_args.iter().cloned());
    // Keep the container alive between attaches regardless of the image's
    // own command.
    out.extend([
        "--entrypoint".into(),
        "sh".into(),
        image,
        "-c".into(),
        "trap 'exit 0' TERM; while :; do sleep 3600 & wait $!; done".into(),
    ]);
    out
}

/// `docker exec` arguments running `argv` in the container.
pub fn exec_args(dc: &DevContainer, container: &str, argv: &[String], tty: bool) -> Vec<String> {
    let mut out = vec!["exec".to_string()];
    if tty {
        out.push("-it".into());
        for var in ["TERM", "COLORTERM"] {
            if std::env::var_os(var).is_some() {
                out.push("-e".into());
                out.push(var.into());
            }
        }
    }
    if let Some(user) = &dc.remote_user {
        out.push("-u".into());
        out.push(user.clone());
    }
    out.push("-w".into());
    out.push(dc.workspace_folder.clone());
    out.push(container.into());
    out.extend(argv.iter().cloned());
    out
}

fn docker(args: &[String]) -> Result<std::process::ExitStatus> {
    Command::new("docker")
        .args(args)
        .status()
        .context("running docker (is it installed and on PATH?)")
}

fn docker_out(args: &[&str]) -> Result<String> {
    let out = Command::new("docker")
        .args(args)
        .stderr(Stdio::inherit())
        .output()
        .context("running docker (is it installed and on PATH?)")?;
    if !out.status.success() {
        bail!("docker {} exited with {}", args.join(" "), out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn step(msg: &str) {
    eprintln!("devcontainer: {msg}");
}

/// The binary to install into a container of architecture `arch`:
/// `$CROFT_DEVCONTAINER_BINARY`, else a static cross-build (as `croft <host>`
/// ships), else this executable.
fn binary_for(arch: &str) -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("CROFT_DEVCONTAINER_BINARY") {
        return Ok(PathBuf::from(p));
    }
    if let Some(triple) = crate::remote::arch_to_musl_triple(arch) {
        match crate::remote::cross_build_unavailable(triple) {
            None => {
                let (tx, rx) = std::sync::mpsc::channel::<String>();
                let printer = std::thread::spawn(move || {
                    for line in rx {
                        eprintln!("  {line}");
                    }
                });
                let built = crate::remote::cross_build_static(triple, &tx);
                drop(tx);
                let _ = printer.join();
                return built;
            }
            Some(reason) => step(&format!(
                "static build unavailable ({reason}); installing this croft binary instead"
            )),
        }
    }
    std::env::current_exe().context("locating the croft executable")
}

/// Bring up `root`'s dev container and run croft in it on this terminal.
pub fn up_and_attach(root: &Path, rebuild: bool) -> Result<()> {
    let root = root
        .canonicalize()
        .with_context(|| format!("opening {}", root.display()))?;
    let Some(config) = find_config(&root) else {
        bail!(
            "no .devcontainer/devcontainer.json or .devcontainer.json in {}",
            root.display()
        );
    };
    let text = std::fs::read_to_string(&config)
        .with_context(|| format!("reading {}", config.display()))?;
    let dc = parse(&text, &config, &root, &|k| std::env::var(k).ok())?;
    let config_hash = hash(&text);

    let label = format!("label={WORKSPACE_LABEL}={}", root.display());
    let mut container = docker_out(&["ps", "-aq", "--filter", &label])?
        .lines()
        .next()
        .map(str::to_string);
    if let Some(id) = container.clone() {
        let made_from = docker_out(&[
            "inspect",
            "-f",
            &format!("{{{{index .Config.Labels \"{CONFIG_LABEL}\"}}}}"),
            &id,
        ])
        .unwrap_or_default();
        if rebuild || made_from != config_hash {
            step("config changed; replacing the container");
            docker_out(&["rm", "-f", &id])?;
            container = None;
        }
    }

    let container = match container {
        Some(id) => {
            if docker_out(&["inspect", "-f", "{{.State.Running}}", &id])? != "true" {
                step("starting the container");
                docker_out(&["start", &id])?;
            }
            id
        }
        None => {
            if let Some(args) = build_args(&dc, &root) {
                step("building the image");
                let status = docker(&args)?;
                if !status.success() {
                    bail!("docker build exited with {status}");
                }
            }
            step("creating the container");
            let args = run_args(&dc, &root, &config_hash);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let id = docker_out(&refs)?;
            for argv in &dc.post_create {
                step(&format!("postCreateCommand: {}", argv.join(" ")));
                let status = docker(&exec_args(&dc, &id, argv, false))?;
                if !status.success() {
                    bail!("postCreateCommand exited with {status}");
                }
            }
            id
        }
    };

    let arch = docker_out(&["exec", &container, "uname", "-m"])?;
    let binary = binary_for(&arch)?;
    step(&format!("installing {}", binary.display()));
    docker_out(&[
        "cp",
        &binary.display().to_string(),
        &format!("{container}:{CONTAINER_BINARY}"),
    ])?;
    let probe = exec_args(
        &dc,
        &container,
        &[CONTAINER_BINARY.into(), "--version".into()],
        false,
    );
    let refs: Vec<&str> = probe.iter().map(String::as_str).collect();
    if docker_out(&refs).is_err() {
        bail!(
            "the installed croft does not run in this image (likely a libc mismatch); \
             run `croft setup-cross` for a static build, or point CROFT_DEVCONTAINER_BINARY at one"
        );
    }

    let status = docker(&exec_args(
        &dc,
        &container,
        &[CONTAINER_BINARY.into(), dc.workspace_folder.clone()],
        true,
    ))?;
    step(&format!(
        "left the container running; `docker rm -f {}` removes it",
        &container[..container.len().min(12)]
    ));
    if !status.success() {
        bail!("croft in the container exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(text: &str) -> DevContainer {
        let root = Path::new("/home/me/proj");
        parse(
            text,
            &root.join(".devcontainer/devcontainer.json"),
            root,
            &|k| (k == "USER").then(|| "me".to_string()),
        )
        .unwrap()
    }

    #[test]
    fn an_image_config_with_comments_and_trailing_commas_parses() {
        let dc = parse_str(
            r#"{
              // the image
              "image": "mcr.microsoft.com/devcontainers/rust:1", /* pinned */
              "forwardPorts": [3000, "8080", "db:5432"],
              "postCreateCommand": "cargo fetch",
              "containerEnv": { "WHO": "${localEnv:USER}" },
              "remoteUser": "vscode",
            }"#,
        );
        assert_eq!(
            dc.source,
            Source::Image("mcr.microsoft.com/devcontainers/rust:1".into())
        );
        assert_eq!(dc.ports, vec!["127.0.0.1:3000:3000", "127.0.0.1:8080:8080"]);
        assert_eq!(
            dc.post_create,
            vec![vec!["sh".to_string(), "-c".into(), "cargo fetch".into()]]
        );
        assert_eq!(dc.env, vec![("WHO".to_string(), "me".to_string())]);
        assert_eq!(dc.workspace_folder, "/workspaces/proj");
        assert_eq!(
            dc.workspace_mount,
            "type=bind,source=/home/me/proj,target=/workspaces/proj"
        );
        assert_eq!(dc.remote_user.as_deref(), Some("vscode"));
    }

    #[test]
    fn build_paths_are_relative_to_the_config_and_mounts_are_substituted() {
        let dc = parse_str(
            r#"{
              "build": { "dockerfile": "Dockerfile", "context": "..", "args": { "V": "1" } },
              "mounts": [
                "source=${localWorkspaceFolder}/.cache,target=/cache,type=bind",
                { "source": "vol", "target": "${containerWorkspaceFolder}/target", "type": "volume" }
              ],
              "postCreateCommand": { "a": ["make", "deps"], "b": "echo hi" }
            }"#,
        );
        assert_eq!(
            dc.source,
            Source::Build {
                dockerfile: PathBuf::from("/home/me/proj/.devcontainer/Dockerfile"),
                context: PathBuf::from("/home/me/proj/.devcontainer/.."),
                args: vec![("V".into(), "1".into())],
            }
        );
        assert_eq!(
            dc.mounts,
            vec![
                "source=/home/me/proj/.cache,target=/cache,type=bind".to_string(),
                "type=volume,source=vol,target=/workspaces/proj/target".into(),
            ]
        );
        assert_eq!(dc.post_create.len(), 2);
        assert_eq!(dc.post_create[0], vec!["make", "deps"]);
    }

    #[test]
    fn a_config_without_image_or_build_is_refused() {
        let root = Path::new("/p");
        assert!(parse("{}", &root.join(".devcontainer.json"), root, &|_| None).is_err());
    }

    #[test]
    fn docker_arguments_carry_the_config() {
        let root = Path::new("/home/me/proj");
        let dc = parse_str(
            r#"{ "build": { "dockerfile": "Dockerfile" }, "forwardPorts": [3000],
                 "mounts": ["type=volume,target=/x"], "runArgs": ["--cap-add=SYS_PTRACE"] }"#,
        );
        let build = build_args(&dc, root).unwrap();
        let tag = image_tag(root);
        assert_eq!(build[..3], ["build".to_string(), "-t".into(), tag.clone()]);
        assert_eq!(build.last().unwrap(), "/home/me/proj/.devcontainer/.");

        let run = run_args(&dc, root, "abc");
        let joined = run.join(" ");
        assert!(joined.contains("--label croft.devcontainer=/home/me/proj"));
        assert!(joined.contains("--label croft.devcontainer.config=abc"));
        assert!(joined.contains("-p 127.0.0.1:3000:3000"));
        assert!(joined.contains("--mount type=volume,target=/x"));
        assert!(joined.contains("--cap-add=SYS_PTRACE"));
        assert!(
            joined.contains(&format!("--entrypoint sh {tag} -c")),
            "{joined}"
        );

        let exec = exec_args(&dc, "c1", &["ls".into()], false);
        assert_eq!(exec, vec!["exec", "-w", "/workspaces/proj", "c1", "ls"]);
    }

    #[test]
    fn unknown_variables_are_left_alone() {
        let s = substitute(
            "${nope}/${localWorkspaceFolderBasename}",
            Path::new("/a/b"),
            "",
            &|_| None,
        );
        assert_eq!(s, "${nope}/b");
        assert_eq!(
            substitute("${localEnv:MISSING:dflt}", Path::new("/"), "", &|_| None),
            "dflt"
        );
    }
}
