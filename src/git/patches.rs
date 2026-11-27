//! Applying a hunk or line patch to the index or the working tree, keeping a file's CRLF
//! line endings as they are.

use super::*;

#[derive(Clone, Copy)]
pub(super) enum HunkSide {
    Old,
    New,
}

/// `patch` with each `@@ -a,b +c,d @@` header's two starts set to the
/// `side` one, counts untouched.
pub(super) fn retarget_hunk_starts(patch: &str, side: HunkSide) -> String {
    let fix = |line: &str| -> Option<String> {
        let rest = line.strip_prefix("@@ -")?;
        let (old, rest) = rest.split_once(" +")?;
        let (new, tail) = rest.split_once(" @@")?;
        let start = |range: &str| range.split(',').next().map(str::to_string);
        let count = |range: &str| range.split_once(',').map(|(_, c)| format!(",{c}"));
        let pick = match side {
            HunkSide::Old => start(old)?,
            HunkSide::New => start(new)?,
        };
        Some(format!(
            "@@ -{pick}{} +{pick}{} @@{tail}",
            count(old).unwrap_or_default(),
            count(new).unwrap_or_default()
        ))
    };
    // Split on `\n` only: a CRLF file's patch lines end in `\r`, which
    // `str::lines` would strip.
    patch
        .split('\n')
        .map(|l| fix(l).unwrap_or_else(|| l.to_string()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The path a single-file patch writes, from its `+++ b/<path>` header.
pub(super) fn patch_path(patch: &str) -> Option<&str> {
    patch
        .split('\n')
        .find_map(|l| l.strip_prefix("+++ b/"))
        .map(|p| p.trim_end_matches('\r'))
}

/// Whether `git add` turns this path's CRLF line endings into LF in the
/// index, whatever decides it (`.gitattributes` `eol`/`text`,
/// `core.autocrlf`, `core.eol`): git cleans a probe line for the path and
/// the result is compared with the plain LF line.
pub(super) fn strips_cr_on_add(root: &Path, rel: &str) -> bool {
    let hash = |input: &[u8], args: &[&str]| -> Option<String> {
        use std::io::Write as _;
        let mut child = Git::new()
            .at(root)
            .args(["hash-object", "--stdin"])
            .args(args)
            .into_command()
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        child.stdin.take()?.write_all(input).ok()?;
        let out = child.wait_with_output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let path_arg = format!("--path={rel}");
    match (
        hash(b"croft\r\n", &[&path_arg]),
        hash(b"croft\n", &["--no-filters"]),
    ) {
        (Some(cleaned), Some(lf)) => cleaned == lf,
        _ => false,
    }
}

/// `patch` with the `\r` dropped from the end of every added line.
pub(super) fn strip_added_crs(patch: &str) -> String {
    patch
        .split('\n')
        .map(|l| {
            if l.starts_with('+') && !l.starts_with("+++ ") {
                l.strip_suffix('\r').unwrap_or(l)
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `patch` with a `\r` put back on every context and removed line (the
/// ones taken from HEAD), to match a CRLF working tree.
pub(super) fn add_crs_to_head_lines(patch: &str) -> String {
    patch
        .split('\n')
        .map(|l| {
            let from_head = (l.starts_with(' ') || l.starts_with('-')) && !l.starts_with("--- ");
            if from_head && !l.ends_with('\r') {
                format!("{l}\r")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Apply a unified-diff patch fed on stdin via `git apply`. `cached`
/// targets the index (stage), `reverse` un-applies (`cached + reverse` =
/// unstage, `reverse` alone = revert the working tree). Logged to the
/// OUTPUT panel's Git channel like every other mutation; the caller gets
/// git's verbatim error when a hunk no longer applies.
pub fn apply_patch(
    root: &Path,
    patch: &str,
    cached: bool,
    reverse: bool,
) -> Result<String, String> {
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    let mut args: Vec<&str> = vec!["apply", "--whitespace=nowarn"];
    if cached {
        args.push("--cached");
    }
    if reverse {
        args.push("-R");
    }
    args.push("-");
    // The diff's `@@ -old +new @@` counts HEAD lines on one side and
    // working-tree lines on the other, and `git apply` starts its offset
    // search at the side it writes to. A single-hunk patch lands on a target
    // where no earlier hunk was applied, so each side's number is right for
    // one target only: staging writes the index (HEAD's numbering), a revert
    // writes the working tree (its own). With the other number, a file with
    // repeated context took the patch at the wrong place.
    // `git apply` patches a file in git's clean form: `--cached` writes the
    // result into the index as it is, skipping the clean filter `git add`
    // runs, so a hunk of an LFS file put the raw content into git where a
    // pointer belongs; on the working tree the patch is matched against the
    // cleaned text (the pointer) and never applies (#1238).
    let paths = patch.lines().filter_map(|l| {
        l.strip_prefix("+++ b/")
            .or_else(|| l.strip_prefix("--- a/"))
    });
    for rel in paths {
        if let Some(filter) = path_filter(root, rel) {
            let whole = match (cached, reverse) {
                (true, false) => "stage",
                (true, true) => "unstage",
                _ => "discard",
            };
            return Err(format!(
                "{rel} uses the {filter} filter: {whole} the whole file, not a part"
            ));
        }
    }
    let retargeted = match (cached, reverse) {
        (true, false) => retarget_hunk_starts(patch, HunkSide::Old),
        (false, true) => retarget_hunk_starts(patch, HunkSide::New),
        _ => patch.to_string(),
    };
    // A path git stores LF but checks out CRLF (`eol=crlf`,
    // `core.autocrlf`) has two spellings, and the patch mixes them: its
    // added lines come from the working tree (CRLF), its context and
    // removed lines from HEAD (LF). `git apply` uses no conversion, so each
    // target needs the patch in its own spelling (#1236): the index gets
    // the added lines cleaned, as `git add` would (verbatim, a CRLF
    // checkout's `\r` made the blob mixed), and the working tree gets the
    // HEAD lines with its `\r` (without, a revert did not apply at all).
    let retargeted = match patch_path(&retargeted) {
        Some(rel) if strips_cr_on_add(root, rel) => {
            if cached {
                strip_added_crs(&retargeted)
            } else if std::fs::read(root.join(rel))
                .is_ok_and(|b| b.windows(2).any(|w| w == b"\r\n"))
            {
                add_crs_to_head_lines(&retargeted)
            } else {
                retargeted
            }
        }
        _ => retargeted,
    };
    let patch = retargeted.as_str();
    crate::output::push(
        crate::output::CHANNEL_GIT,
        crate::output::OutputLevel::Info,
        &format!("> git {} <<patch", args.join(" ")),
    );
    let mut child = Git::new()
        .at(path_str)
        .args(&args)
        .into_command()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if let Some(stdin) = child.stdin.take() {
        use std::io::Write;
        let mut stdin = stdin;
        if let Err(e) = stdin.write_all(patch.as_bytes()) {
            // Reap before bailing or the failed child lingers as a zombie.
            drop(stdin);
            let _ = child.wait();
            return Err(format!("failed to feed patch to git apply: {e}"));
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("git apply did not exit cleanly: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        Ok(if !stdout.is_empty() { stdout } else { stderr })
    } else {
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        let err = if msg.is_empty() {
            format!("git apply failed with code {:?}", output.status.code())
        } else {
            msg
        };
        crate::output::push(
            crate::output::CHANNEL_GIT,
            crate::output::OutputLevel::Error,
            &err,
        );
        Err(err)
    }
}
