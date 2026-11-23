//! `${...}` variables: what they resolve against, and expanding them in a string or a
//! whole configuration, with an unknown one a hard error.

use super::*;

/// The editor state `${...}` variables resolve against.
#[derive(Debug, Clone)]
pub struct SubstCtx {
    pub workspace_folder: PathBuf,
    /// Active editor file; `None` makes `${file}`-family variables error.
    pub file: Option<PathBuf>,
    /// The user's answers to `${input:id}` (#1424), by id.
    pub inputs: BTreeMap<String, String>,
    /// The workspace's folders as `(name, root)`, for
    /// `${workspaceFolder:name}` (#1425).
    pub folders: Vec<(String, PathBuf)>,
}

/// Expand the supported `${...}` variables in `input`. An unknown or
/// unresolvable variable is a hard error naming it — VS Code launches with
/// the literal text and the program fails obscurely; croft refuses loudly
/// instead (issue #250 acceptance).
pub fn substitute(input: &str, ctx: &SubstCtx) -> Result<String, String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            // No closing brace: not a variable, keep the text as written.
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let var = &after[..end];
        out.push_str(&expand_variable(var, ctx)?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

pub(super) fn expand_variable(var: &str, ctx: &SubstCtx) -> Result<String, String> {
    let file = || {
        ctx.file
            .as_deref()
            .ok_or_else(|| format!("${{{var}}}: no active file"))
    };
    match var {
        "workspaceFolder" | "workspaceRoot" => {
            Ok(ctx.workspace_folder.to_string_lossy().into_owned())
        }
        "workspaceFolderBasename" => Ok(ctx
            .workspace_folder
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()),
        "file" => Ok(file()?.to_string_lossy().into_owned()),
        "fileBasename" => Ok(file()?
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()),
        "fileBasenameNoExtension" => Ok(file()?
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()),
        "fileDirname" => Ok(file()?
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()),
        "fileExtname" => Ok(file()?
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default()),
        "cwd" => Ok(ctx.workspace_folder.to_string_lossy().into_owned()),
        "pathSeparator" => Ok(String::from(std::path::MAIN_SEPARATOR_STR)),
        _ => {
            if let Some(id) = var.strip_prefix("input:") {
                return ctx
                    .inputs
                    .get(id.trim())
                    .cloned()
                    .ok_or_else(|| format!("${{{var}}}: no answer for this input"));
            }
            if let Some(name) = var.strip_prefix("env:") {
                return std::env::var(name)
                    .map_err(|_| format!("${{env:{name}}}: environment variable not set"));
            }
            // `${workspaceFolder:web}` (#1425): the root of the workspace
            // folder named `web`, as multi-root configurations write it.
            if let Some(name) = var.strip_prefix("workspaceFolder:") {
                return folder_root(&ctx.folders, name)
                    .map(|r| r.to_string_lossy().into_owned())
                    .ok_or_else(|| format!("${{{var}}}: no workspace folder is named \"{name}\""));
            }
            Err(format!("${{{var}}}: unsupported substitution variable"))
        }
    }
}

/// Recursively expand `${...}` in every string of `v` (object values and
/// array elements included), so passthrough fields get substitution too.
pub(super) fn substitute_value(v: &Value, ctx: &SubstCtx) -> Result<Value, String> {
    Ok(match v {
        Value::String(s) => Value::String(substitute(s, ctx)?),
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|x| substitute_value(x, ctx))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| Ok((k.clone(), substitute_value(x, ctx)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}
