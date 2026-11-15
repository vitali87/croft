//! Dev Containers (#617): `croft devcontainer [path] --config <name>`.
//!
//! Reads the workspace's `.devcontainer/devcontainer.json` (or
//! `.devcontainer.json`). A workspace with several configs keeps each in
//! `.devcontainer/<name>/devcontainer.json`. `--config <name>` selects one
//! (`list_configs`). With none given, the only config is used, or the names
//! are listed. It builds or
//! pulls the image, starts a container with the workspace mounted, runs
//! `postCreateCommand` once, installs a croft binary into it the way
//! `croft <host>` provisions a remote (a static musl cross-build when the
//! toolchain is there), then runs croft inside it on this terminal.
//!
//! The container is labelled with the workspace path, the config name and a
//! hash of the config, so the next run reattaches to that config's container
//! (starting it if stopped) instead of creating another. An edited config is
//! only reported (#1110): replacing the container is `--rebuild`'s job, since
//! it throws away everything made inside it, and kills a croft still running
//! there with its unsaved edits.
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
/// Label naming which config it is. See `list_configs`.
const ID_LABEL: &str = "croft.devcontainer.id";
/// Id of `.devcontainer/devcontainer.json`.
const FLAT_CONFIG_ID: &str = "single/folder";
/// Id of `.devcontainer.json` at the workspace root.
const ROOT_CONFIG_ID: &str = "single/file";
/// Label holding the hash of the config the container was made from.
const CONFIG_LABEL: &str = "croft.devcontainer.config";

/// One `devcontainer.json` in a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFile {
    /// See `list_configs`.
    pub id: String,
    pub path: PathBuf,
}

/// Every dev container config in `root`, in id order.
pub fn list_configs(root: &Path) -> Vec<ConfigFile> {
    let flat = [
        root.join(".devcontainer").join("devcontainer.json"),
        root.join(".devcontainer.json"),
    ];
    let flats: Vec<PathBuf> = flat.into_iter().filter(|p| p.is_file()).collect();
    let mut named = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root.join(".devcontainer")) {
        for ent in rd.flatten() {
            if !ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let path = ent.path().join("devcontainer.json");
            if path.is_file() {
                if let Some(id) = ent.file_name().to_str() {
                    named.push(ConfigFile {
                        id: id.to_string(),
                        path,
                    });
                }
            }
        }
    }
    let mut out: Vec<ConfigFile> = flats
        .into_iter()
        .map(|path| {
            let id = if path.ends_with(".devcontainer.json") && path.parent() == Some(root) {
                String::from(ROOT_CONFIG_ID)
            } else {
                String::from(FLAT_CONFIG_ID)
            };
            ConfigFile { id, path }
        })
        .chain(named)
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// The config `name` asks for, or the only one when `name` is absent.
pub fn resolve_config(root: &Path, name: Option<&str>) -> Result<ConfigFile> {
    let configs = list_configs(root);
    if configs.is_empty() {
        bail!(
            "no .devcontainer/devcontainer.json, .devcontainer/<name>/devcontainer.json \
             or .devcontainer.json in {}",
            root.display()
        );
    }
    match name {
        None if configs.len() == 1 => Ok(configs.into_iter().next().unwrap()),
        None => bail!(
            "{}",
            config_choices(
                "several dev containers; pass --config with one of",
                &configs
            )
        ),
        Some(want) => {
            let hits: Vec<_> = configs.iter().filter(|c| c.id == want).cloned().collect();
            match hits.len() {
                1 => Ok(hits.into_iter().next().unwrap()),
                0 => bail!(
                    "{}",
                    config_choices(
                        &format!("no dev container named `{want}`; pass --config with one of"),
                        &configs
                    )
                ),
                _ => bail!(
                    "more than one dev container is named `{want}`:\n{}",
                    hits.iter()
                        .map(|c| format!("  {}", c.path.display()))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            }
        }
    }
}

/// `id - <json name>` lines, the json `name` omitted when it matches the id
/// or the file does not parse.
fn config_choices(head: &str, configs: &[ConfigFile]) -> String {
    let mut out = format!("{head}:");
    for c in configs {
        let id = &c.id;
        match display_name(&c.path) {
            Some(title) if title != c.id => out.push_str(&format!("\n  {id} - {title}")),
            _ => out.push_str(&format!("\n  {id}")),
        }
    }
    out
}

/// An unlabelled container (made before configs had ids) belongs to a flat
/// file only. The flat `.devcontainer/devcontainer.json` owns it whenever
/// that file exists; `.devcontainer.json` owns it only when it is the only
/// flat file. A folder named `default` never does.
fn reuses_unlabelled_container(root: &Path, config: &ConfigFile) -> bool {
    let primary = root.join(".devcontainer").join("devcontainer.json");
    if config.path == primary {
        return true;
    }
    config.path == root.join(".devcontainer.json") && !primary.is_file()
}

fn display_name(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&crate::jsonc::strip_jsonc(&text)).ok()?;
    let name = v.get("name").and_then(Value::as_str)?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

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
    let v: Value = serde_json::from_str(&crate::jsonc::strip_jsonc(text))
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
                    Some(source) => format!(
                        "type={kind},{},{}",
                        mount_field("source", &source),
                        mount_field("target", &target)
                    ),
                    None => format!("type={kind},{}", mount_field("target", &target)),
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
            "type=bind,{},{}",
            mount_field("source", &root.display().to_string()),
            mount_field("target", &workspace_folder)
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

/// `key=value` for a docker `--mount` spec, CSV-quoted when the value has
/// a comma or a quote: docker splits the spec as CSV, so `/home/u/a,b`
/// would otherwise read as a stray field `b` and the run would fail.
fn mount_field(key: &str, value: &str) -> String {
    if value.contains([',', '"']) {
        format!("\"{key}={}\"", value.replace('"', "\"\""))
    } else {
        format!("{key}={value}")
    }
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

/// The tag a `build` config's image gets. `id` keeps two configs in one
/// workspace from sharing a tag.
fn image_tag(root: &Path, id: &str) -> String {
    format!(
        "croft-devcontainer-{}",
        &hash(&format!("{}\0{id}", root.display()))[..12]
    )
}

/// `docker build` arguments for a `build` config, `None` for an `image`.
pub fn build_args(dc: &DevContainer, root: &Path, id: &str) -> Option<Vec<String>> {
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
        image_tag(root, id),
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
pub fn run_args(dc: &DevContainer, root: &Path, id: &str, config_hash: &str) -> Vec<String> {
    let image = match &dc.source {
        Source::Image(i) => i.clone(),
        Source::Build { .. } => image_tag(root, id),
    };
    let mut out = vec![
        "run".to_string(),
        "-d".into(),
        "--init".into(),
        "--label".into(),
        format!("{WORKSPACE_LABEL}={}", root.display()),
        "--label".into(),
        format!("{ID_LABEL}={id}"),
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

/// The hash a container is labelled with: of what `docker build` and
/// `docker run` are given and the `postCreateCommand`s, not of the file's
/// text (#1110), so a comment or a reformat is no change. Keys used only at
/// attach time, such as `remoteUser`, need no new container and are left out.
fn config_hash(dc: &DevContainer, root: &Path, id: &str) -> String {
    // `containerEnv` and `build.args` keep the file's key order, which
    // means nothing to the container.
    let mut dc = dc.clone();
    dc.env.sort();
    if let Source::Build { args, .. } = &mut dc.source {
        args.sort();
    }
    let mut parts = run_args(&dc, root, id, "");
    parts.extend(build_args(&dc, root, id).unwrap_or_default());
    for argv in &dc.post_create {
        parts.push(String::from("postCreateCommand"));
        parts.extend(argv.iter().cloned());
    }
    hash(&parts.join("\0"))
}

/// Whether `docker top` output lists a croft running in the container: a
/// session attached from another terminal, which removing the container
/// would kill with its unsaved edits (#1110).
fn croft_attached(top: &str) -> bool {
    top.lines()
        .skip(1)
        .any(|l| l.split_whitespace().any(|w| w == CONTAINER_BINARY))
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
/// `name` selects a config (`list_configs`). Leaving it off uses the only
/// config, or lists them.
pub fn up_and_attach(root: &Path, name: Option<&str>, rebuild: bool) -> Result<()> {
    let root = root
        .canonicalize()
        .with_context(|| format!("opening {}", root.display()))?;
    let config = resolve_config(&root, name)?;
    let id = config.id.clone();
    let text = std::fs::read_to_string(&config.path)
        .with_context(|| format!("reading {}", config.path.display()))?;
    let mut dc = parse(&text, &config.path, &root, &|k| std::env::var(k).ok())?;
    let config_hash = config_hash(&dc, &root, &id);

    let label = format!("label={WORKSPACE_LABEL}={}", root.display());
    let id_label = format!("label={ID_LABEL}={id}");
    let mut container = docker_out(&["ps", "-aq", "--filter", &label, "--filter", &id_label])?
        .lines()
        .next()
        .map(str::to_string);
    // A container made before configs were named carries only the workspace
    // label. Reuse it for the flat file that owned the workspace then, not
    // for `.devcontainer/<name>/` (including a folder named `default`).
    if container.is_none() && reuses_unlabelled_container(&root, &config) {
        if let Ok(ids) = docker_out(&["ps", "-aq", "--filter", &label]) {
            for cid in ids.lines().filter(|l| !l.is_empty()) {
                // Fail closed: an inspect error is not evidence the container
                // has no config id. Only a successful read of an absent label
                // means this is the pre-naming container.
                let got = docker_out(&[
                    "inspect",
                    "-f",
                    &format!("{{{{index .Config.Labels \"{ID_LABEL}\"}}}}"),
                    cid,
                ])?;
                if got.is_empty() || got == "<no value>" {
                    container = Some(cid.to_string());
                    break;
                }
            }
        }
    }
    if let Some(id) = container.clone() {
        if rebuild {
            // Fail closed: a check that can't be made is no evidence that
            // nothing would be lost.
            let unsure = "can't tell whether croft is running in the container, so --rebuild \
                          leaves it in place";
            let running = docker_out(&["inspect", "-f", "{{.State.Running}}", &id])
                .context(unsure)?
                == "true";
            if running && croft_attached(&docker_out(&["top", &id]).context(unsure)?) {
                bail!(
                    "croft is still running in this container, in another terminal; quit it \
                     there first so its unsaved edits are not lost, or remove the container \
                     yourself with `docker rm -f {}`",
                    &id[..id.len().min(12)]
                );
            }
            step("replacing the container (--rebuild)");
            docker_out(&["rm", "-f", &id])?;
            container = None;
        } else {
            let made_from = docker_out(&[
                "inspect",
                "-f",
                &format!("{{{{index .Config.Labels \"{CONFIG_LABEL}\"}}}}"),
                &id,
            ])
            .unwrap_or_default();
            // A croft before #1110 labelled the hash of the file's text.
            if made_from != config_hash && made_from != hash(&text) {
                step(
                    "devcontainer.json changed since this container was created; run \
                     `croft devcontainer --rebuild` to recreate it (this discards changes \
                     made inside the container)",
                );
                // Until it is rebuilt, the workspace sits where the container
                // was made to mount it, not where the edited config says.
                let made_in = docker_out(&["inspect", "-f", "{{.Config.WorkingDir}}", &id])?;
                if !made_in.is_empty() {
                    dc.workspace_folder = made_in;
                }
            }
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
            if let Some(args) = build_args(&dc, &root, &id) {
                step("building the image");
                let status = docker(&args)?;
                if !status.success() {
                    bail!("docker build exited with {status}");
                }
            }
            step("creating the container");
            let args = run_args(&dc, &root, &id, &config_hash);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let id = docker_out(&refs)?;
            for argv in &dc.post_create {
                step(&format!("postCreateCommand: {}", argv.join(" ")));
                let ok = docker(&exec_args(&dc, &id, argv, false));
                if !matches!(&ok, Ok(status) if status.success()) {
                    // A half-set-up container carries the current config hash,
                    // so the next run would reuse it and skip setup: remove it.
                    let _ = docker_out(&["rm", "-f", &id]);
                    let status = ok?;
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

    #[test]
    fn a_mount_path_with_a_comma_is_quoted_for_docker() {
        assert_eq!(mount_field("source", "/w/app"), "source=/w/app");
        assert_eq!(mount_field("source", "/w/a,b"), "\"source=/w/a,b\"");
        assert_eq!(mount_field("source", "/w/a\"b"), "\"source=/w/a\"\"b\"");
    }

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
        let build = build_args(&dc, root, "default").unwrap();
        let tag = image_tag(root, "default");
        assert_eq!(build[..3], ["build".to_string(), "-t".into(), tag.clone()]);
        assert_eq!(build.last().unwrap(), "/home/me/proj/.devcontainer/.");

        let run = run_args(&dc, root, "default", "abc");
        let joined = run.join(" ");
        assert!(joined.contains("--label croft.devcontainer=/home/me/proj"));
        assert!(joined.contains("--label croft.devcontainer.id=default"));
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

    /// #1110: the label's hash is of the container the config makes, so a
    /// comment, a reformat or an attach-time key is no change, and what
    /// `docker run` or setup would do differently is.
    #[test]
    fn the_config_hash_ignores_comments_and_formatting() {
        let root = Path::new("/home/me/proj");
        let of = |text: &str| config_hash(&parse_str(text), root, "default");
        let base = of(r#"{ "image": "ubuntu:24.04", "postCreateCommand": "make" }"#);
        for same in [
            "// Set up.\n{\n  \"image\": \"ubuntu:24.04\",\n  \"postCreateCommand\": \"make\", // why\n}\n",
            r#"{ "postCreateCommand": "make", "image": "ubuntu:24.04" }"#,
            r#"{ "image": "ubuntu:24.04", "postCreateCommand": "make", "remoteUser": "me" }"#,
        ] {
            assert_eq!(of(same), base, "{same}");
        }
        let env = of(r#"{ "image": "u", "containerEnv": { "A": "1", "B": "2" } }"#);
        assert_eq!(
            of(r#"{ "image": "u", "containerEnv": { "B": "2", "A": "1" } }"#),
            env,
            "key order"
        );
        assert_ne!(
            of(r#"{ "image": "u", "containerEnv": { "A": "1", "B": "3" } }"#),
            env
        );
        let build = |args: &str| {
            of(&format!(
                r#"{{ "build": {{ "dockerfile": "Dockerfile", "args": {args} }} }}"#
            ))
        };
        assert_eq!(
            build(r#"{ "X": "1", "Y": "2" }"#),
            build(r#"{ "Y": "2", "X": "1" }"#)
        );
        for changed in [
            r#"{ "image": "ubuntu:26.04", "postCreateCommand": "make" }"#,
            r#"{ "image": "ubuntu:24.04", "postCreateCommand": "make test" }"#,
            r#"{ "image": "ubuntu:24.04", "postCreateCommand": "make", "forwardPorts": [8000] }"#,
        ] {
            assert_ne!(of(changed), base, "{changed}");
        }
    }

    #[test]
    fn a_croft_running_in_the_container_is_found_in_docker_top() {
        let top = |cmd: &str| {
            format!("UID PID PPID C STIME TTY TIME CMD\nroot 1 0 0 10:00 ? 00:00:00 {cmd}\n")
        };
        assert!(croft_attached(&top(
            "/usr/local/bin/croft /workspaces/proj"
        )));
        assert!(!croft_attached(&top(
            "sh -c trap 'exit 0' TERM; while :; do sleep 3600 & wait $!; done"
        )));
        assert!(!croft_attached(&top("/home/me/bin/croft-helper")));
        assert!(!croft_attached(""));
    }

    #[test]
    fn named_configs_are_listed_and_selected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let native = root.join(".devcontainer/native");
        let cross = root.join(".devcontainer/cross");
        std::fs::create_dir_all(&native).unwrap();
        std::fs::create_dir_all(&cross).unwrap();
        std::fs::write(
            native.join("devcontainer.json"),
            r#"{ "name": "Native build", "image": "native" }"#,
        )
        .unwrap();
        std::fs::write(
            cross.join("devcontainer.json"),
            r#"{ "name": "Cross build", "image": "cross" }"#,
        )
        .unwrap();

        let ids: Vec<_> = list_configs(root).into_iter().map(|c| c.id).collect();
        assert_eq!(ids, ["cross", "native"]);
        assert!(
            resolve_config(root, None)
                .unwrap_err()
                .to_string()
                .contains("native - Native build")
        );
        assert_eq!(
            resolve_config(root, Some("native")).unwrap().path,
            native.join("devcontainer.json")
        );
        assert!(
            resolve_config(root, Some("nope"))
                .unwrap_err()
                .to_string()
                .contains("cross - Cross build")
        );
    }

    #[test]
    fn a_single_flat_config_uses_its_path_as_the_id() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = root.join(".devcontainer");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("devcontainer.json"), r#"{ "image": "u" }"#).unwrap();
        let found = resolve_config(root, None).unwrap();
        assert_eq!(found.id, "single/folder");
        assert_eq!(found.path, dir.join("devcontainer.json"));
        assert_eq!(
            resolve_config(root, Some("single/folder")).unwrap().path,
            dir.join("devcontainer.json")
        );
    }

    #[test]
    fn flat_ids_stay_put_when_another_config_appears() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let flat = root.join(".devcontainer/devcontainer.json");
        let named = root.join(".devcontainer/default/devcontainer.json");
        let root_json = root.join(".devcontainer.json");
        std::fs::create_dir_all(named.parent().unwrap()).unwrap();
        std::fs::write(&flat, r#"{ "name": "Flat", "image": "flat" }"#).unwrap();
        std::fs::write(&named, r#"{ "name": "Named default", "image": "named" }"#).unwrap();
        std::fs::write(&root_json, r#"{ "image": "root" }"#).unwrap();

        let ids: Vec<_> = list_configs(root).into_iter().map(|c| c.id).collect();
        assert_eq!(ids, ["default", "single/file", "single/folder"]);
        assert_eq!(
            resolve_config(root, Some("single/folder")).unwrap().path,
            flat
        );
        assert_eq!(
            resolve_config(root, Some("single/file")).unwrap().path,
            root_json
        );
        assert_eq!(resolve_config(root, Some("default")).unwrap().path, named);
        let listed = resolve_config(root, None).unwrap_err().to_string();
        assert!(listed.contains("single/folder - Flat"), "{listed}");
        assert!(reuses_unlabelled_container(
            root,
            &resolve_config(root, Some("single/folder")).unwrap()
        ));
        assert!(!reuses_unlabelled_container(
            root,
            &resolve_config(root, Some("default")).unwrap()
        ));
        assert!(!reuses_unlabelled_container(
            root,
            &resolve_config(root, Some("single/file")).unwrap()
        ));
    }

    #[test]
    fn a_lone_root_devcontainer_json_keeps_the_unlabelled_container() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let path = root.join(".devcontainer.json");
        std::fs::write(&path, r#"{ "image": "root" }"#).unwrap();
        let found = resolve_config(root, None).unwrap();
        assert_eq!(found.id, "single/file");
        assert!(reuses_unlabelled_container(root, &found));
        assert!(resolve_config(root, Some("single/folder")).is_err());
    }

    #[test]
    fn image_tags_differ_by_config_name() {
        let root = Path::new("/home/me/proj");
        assert_ne!(image_tag(root, "native"), image_tag(root, "cross"));
        let dc = parse_str(r#"{ "build": { "dockerfile": "Dockerfile" } }"#);
        let native = run_args(&dc, root, "native", "abc").join(" ");
        let cross = run_args(&dc, root, "cross", "abc").join(" ");
        assert!(native.contains("--label croft.devcontainer.id=native"));
        assert!(cross.contains("--label croft.devcontainer.id=cross"));
        assert_ne!(native, cross);
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
