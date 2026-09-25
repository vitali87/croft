//! Dev Containers (#617): the container `.devcontainer/devcontainer.json`
//! describes, as `docker` commands croft runs to build or start it.
//!
//! First the fields that decide whether a workspace works at all: `image`
//! or `build`, `mounts`, `forwardPorts`, `postCreateCommand`, plus
//! `workspaceFolder`, `remoteUser` and `containerEnv`. Pure: argument lists
//! out, the caller runs them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `build` in devcontainer.json.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Build {
    /// Relative to the `.devcontainer` folder.
    pub dockerfile: String,
    /// Relative to the `.devcontainer` folder; `.` when absent.
    pub context: String,
    pub args: BTreeMap<String, String>,
}

/// What devcontainer.json asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spec {
    pub name: String,
    pub image: Option<String>,
    pub build: Option<Build>,
    /// Docker `--mount` values, as written.
    pub mounts: Vec<String>,
    pub forward_ports: Vec<u16>,
    /// A shell command line; an array form is joined with spaces, each
    /// word quoted.
    pub post_create: Option<String>,
    pub workspace_folder: String,
    pub remote_user: Option<String>,
    pub container_env: BTreeMap<String, String>,
}

/// Where devcontainer.json lives under `root`, if it does: the
/// `.devcontainer` folder first, then `.devcontainer.json` at the root.
pub fn find(root: &Path) -> Option<PathBuf> {
    [
        root.join(".devcontainer").join("devcontainer.json"),
        root.join(".devcontainer.json"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// Quote `s` for a POSIX shell.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn string_map(v: Option<&serde_json::Value>) -> BTreeMap<String, String> {
    v.and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| {
                    let v = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    Some((k.clone(), v))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Read devcontainer.json (comments and trailing commas allowed). An error
/// names what is missing or wrong.
pub fn parse(text: &str, root: &Path) -> Result<Spec, String> {
    let v: serde_json::Value = serde_json::from_str(&crate::tasks::strip_jsonc(text))
        .map_err(|e| format!("devcontainer.json does not parse: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| String::from("devcontainer.json is not a JSON object"))?;
    for unsupported in ["dockerComposeFile", "features"] {
        if obj.contains_key(unsupported) {
            return Err(format!(
                "\"{unsupported}\" is not supported yet; use \"image\" or \"build\""
            ));
        }
    }
    let text_of = |k: &str| obj.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let build = obj.get("build").and_then(|b| b.as_object()).map(|b| Build {
        dockerfile: b
            .get("dockerfile")
            .and_then(|v| v.as_str())
            .unwrap_or("Dockerfile")
            .to_string(),
        context: b
            .get("context")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string(),
        args: string_map(b.get("args")),
    });
    let image = text_of("image");
    if image.is_none() && build.is_none() {
        return Err(String::from(
            "devcontainer.json needs an \"image\" or a \"build\" section",
        ));
    }
    let forward_ports = obj
        .get("forwardPorts")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|p| match p {
                    serde_json::Value::Number(n) => n.as_u64().and_then(|n| u16::try_from(n).ok()),
                    serde_json::Value::String(s) => s.parse().ok(),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let mounts = obj
        .get("mounts")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let post_create = match obj.get("postCreateCommand") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Array(a)) => Some(
            a.iter()
                .filter_map(|w| w.as_str())
                .map(quote)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    };
    let folder_name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("workspace"));
    Ok(Spec {
        name: text_of("name").unwrap_or_else(|| folder_name.clone()),
        image,
        build,
        mounts,
        forward_ports,
        post_create,
        workspace_folder: text_of("workspaceFolder")
            .unwrap_or_else(|| format!("/workspaces/{folder_name}")),
        remote_user: text_of("remoteUser"),
        container_env: string_map(obj.get("containerEnv")),
    })
}

/// A stable container name for `root`, so a second run finds the first
/// one's container.
pub fn container_name(root: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut h);
    let slug: String = root
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("croft-devcontainer-{slug}-{:08x}", h.finish() as u32)
}

/// `docker build` arguments for a `build` spec, tagging the image as
/// `tag`; `config_dir` is the folder devcontainer.json is in.
pub fn build_args(build: &Build, config_dir: &Path, tag: &str) -> Vec<String> {
    let mut out = vec![
        String::from("build"),
        String::from("-f"),
        config_dir.join(&build.dockerfile).display().to_string(),
        String::from("-t"),
        tag.to_string(),
    ];
    for (k, v) in &build.args {
        out.push(String::from("--build-arg"));
        out.push(format!("{k}={v}"));
    }
    out.push(config_dir.join(&build.context).display().to_string());
    out
}

/// `docker run` arguments starting the container detached, with the
/// workspace mounted at `workspace_folder` and kept alive.
pub fn run_args(spec: &Spec, root: &Path, name: &str, image: &str) -> Vec<String> {
    let mut out: Vec<String> = [
        "run",
        "-d",
        "--name",
        name,
        "--label",
        &format!("croft.devcontainer={}", root.display()),
        "--mount",
        &format!(
            "type=bind,source={},target={}",
            root.display(),
            spec.workspace_folder
        ),
        "-w",
        &spec.workspace_folder,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for m in &spec.mounts {
        out.push(String::from("--mount"));
        out.push(m.clone());
    }
    for p in &spec.forward_ports {
        out.push(String::from("-p"));
        out.push(format!("{p}:{p}"));
    }
    for (k, v) in &spec.container_env {
        out.push(String::from("-e"));
        out.push(format!("{k}={v}"));
    }
    out.push(image.to_string());
    out.push(String::from("sleep"));
    out.push(String::from("infinity"));
    out
}

/// `docker exec` arguments running `command` in the workspace folder, as
/// the remote user when there is one; `interactive` adds a terminal.
pub fn exec_args(spec: &Spec, name: &str, command: &[String], interactive: bool) -> Vec<String> {
    let mut out = vec![String::from("exec")];
    if interactive {
        out.push(String::from("-it"));
    }
    if let Some(u) = &spec.remote_user {
        out.push(String::from("-u"));
        out.push(u.clone());
    }
    out.push(String::from("-w"));
    out.push(spec.workspace_folder.clone());
    out.push(name.to_string());
    out.extend(command.iter().cloned());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/work/my app")
    }

    #[test]
    fn devcontainer_json_is_found_in_either_place() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(find(tmp.path()), None);
        std::fs::write(tmp.path().join(".devcontainer.json"), "{}").unwrap();
        assert_eq!(
            find(tmp.path()),
            Some(tmp.path().join(".devcontainer.json"))
        );
        std::fs::create_dir_all(tmp.path().join(".devcontainer")).unwrap();
        std::fs::write(tmp.path().join(".devcontainer/devcontainer.json"), "{}").unwrap();
        assert_eq!(
            find(tmp.path()),
            Some(tmp.path().join(".devcontainer/devcontainer.json")),
            "the folder wins"
        );
    }

    #[test]
    fn an_image_spec_parses_with_defaults() {
        let s = parse(
            r#"{
              // a comment
              "name": "Rust",
              "image": "mcr.microsoft.com/devcontainers/rust:1",
              "forwardPorts": [3000, "8080"],
              "mounts": ["source=cache,target=/cache,type=volume"],
              "postCreateCommand": "cargo fetch",
              "containerEnv": {"RUST_LOG": "debug"},
            }"#,
            &root(),
        )
        .unwrap();
        assert_eq!(s.name, "Rust");
        assert_eq!(
            s.image.as_deref(),
            Some("mcr.microsoft.com/devcontainers/rust:1")
        );
        assert_eq!(s.forward_ports, [3000, 8080]);
        assert_eq!(s.mounts, ["source=cache,target=/cache,type=volume"]);
        assert_eq!(s.post_create.as_deref(), Some("cargo fetch"));
        assert_eq!(
            s.workspace_folder, "/workspaces/my app",
            "named after the folder"
        );
        assert_eq!(
            s.container_env.get("RUST_LOG").map(String::as_str),
            Some("debug")
        );
    }

    #[test]
    fn a_build_spec_and_an_array_command_parse() {
        let s = parse(
            r#"{"build": {"dockerfile": "Dockerfile", "context": "..", "args": {"V": "1"}},
                "postCreateCommand": ["npm", "install", "a b"],
                "workspaceFolder": "/src", "remoteUser": "dev"}"#,
            &root(),
        )
        .unwrap();
        assert_eq!(
            s.build,
            Some(Build {
                dockerfile: "Dockerfile".into(),
                context: "..".into(),
                args: BTreeMap::from([("V".into(), "1".into())]),
            })
        );
        assert_eq!(s.post_create.as_deref(), Some("'npm' 'install' 'a b'"));
        assert_eq!(s.workspace_folder, "/src");
        assert_eq!(s.remote_user.as_deref(), Some("dev"));
    }

    #[test]
    fn a_spec_without_an_image_or_build_or_with_unsupported_parts_is_refused() {
        let err = parse(r#"{"name": "x"}"#, &root()).unwrap_err();
        assert!(err.contains("image") && err.contains("build"), "{err}");
        let err = parse(r#"{"dockerComposeFile": "compose.yml"}"#, &root()).unwrap_err();
        assert!(err.contains("dockerComposeFile"), "{err}");
        assert!(parse("{ nope", &root()).is_err());
    }

    #[test]
    fn the_container_name_is_stable_per_workspace() {
        let a = container_name(&root());
        assert_eq!(a, container_name(&root()));
        assert_ne!(a, container_name(Path::new("/work/other")));
        assert!(a.starts_with("croft-devcontainer-my-app-"), "{a}");
        assert!(
            a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "{a}"
        );
    }

    #[test]
    fn docker_argument_lists() {
        let b = Build {
            dockerfile: "Dockerfile".into(),
            context: "..".into(),
            args: BTreeMap::from([("V".into(), "1".into())]),
        };
        let cfg = Path::new("/work/my app/.devcontainer");
        assert_eq!(
            build_args(&b, cfg, "croft-dc:x"),
            [
                "build",
                "-f",
                "/work/my app/.devcontainer/Dockerfile",
                "-t",
                "croft-dc:x",
                "--build-arg",
                "V=1",
                "/work/my app/.devcontainer/.."
            ]
        );
        let s = parse(
            r#"{"image": "img", "forwardPorts": [3000], "mounts": ["type=volume,source=c,target=/c"],
                "containerEnv": {"A": "1"}, "remoteUser": "dev"}"#,
            &root(),
        )
        .unwrap();
        assert_eq!(
            run_args(&s, &root(), "c1", "img"),
            [
                "run",
                "-d",
                "--name",
                "c1",
                "--label",
                "croft.devcontainer=/work/my app",
                "--mount",
                "type=bind,source=/work/my app,target=/workspaces/my app",
                "-w",
                "/workspaces/my app",
                "--mount",
                "type=volume,source=c,target=/c",
                "-p",
                "3000:3000",
                "-e",
                "A=1",
                "img",
                "sleep",
                "infinity"
            ]
        );
        assert_eq!(
            exec_args(
                &s,
                "c1",
                &["sh".into(), "-lc".into(), "cargo fetch".into()],
                false
            ),
            [
                "exec",
                "-u",
                "dev",
                "-w",
                "/workspaces/my app",
                "c1",
                "sh",
                "-lc",
                "cargo fetch"
            ]
        );
        assert_eq!(
            exec_args(&s, "c1", &["croft".into(), ".".into()], true)[..3],
            ["exec", "-it", "-u"]
        );
    }
}
