//! `croft theme-import`: converting a VS Code theme, from a file or the marketplace, and
//! installing it.

use super::*;

/// `croft theme-import <file>`: convert a VS Code theme and install it.
///
/// The notes are printed rather than buried: a converted theme fills croft
/// slots the source never named, and a user comparing it against VS Code
/// deserves to know which colours were derived rather than designed.
pub(super) fn theme_import(
    reference: &str,
    theme: Option<&str>,
    id: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    // A FILE that exists is a path; anything else is a marketplace
    // reference. Checked in that order so a file literally named like an
    // extension id still wins — the user pointing at a file they can see
    // should never silently hit the network instead.
    //
    // `is_file`, not `exists`: a DIRECTORY named like an extension id would
    // otherwise be taken as the theme and fail deep inside the converter
    // with a read error, rather than being passed to the marketplace where
    // the user plainly meant it to go.
    //
    // Progress goes to stderr, so stdout carries only the result: the
    // manifest alone under --dry-run, and nothing at all before the install,
    // so a closed stdout cannot cost the user the theme (#1179).
    let local = Path::new(reference);
    let (source, _scratch) = if local.is_file() {
        eprintln!("Importing from {}", local.display());
        (local.to_path_buf(), None)
    } else {
        let (path, dir) = fetch_marketplace_theme(reference, theme)?;
        (path, Some(dir))
    };
    // `_scratch` is held until this function returns, so the archive and
    // everything lifted from it survive the conversion and go immediately
    // after it — on the error path as well, since the guard drops either
    // way. Nothing downloaded outlives this command.
    let converted = crate::vscode_theme::convert_file(&source, id)?;
    if dry_run {
        print!("{}", converted.manifest);
        return Ok(());
    }
    let installed = crate::vscode_theme::install(&converted)?;
    println!(
        "Imported \"{}\" as theme id {}",
        converted.label, converted.id
    );
    println!(
        "  {}{}",
        installed.path.display(),
        if installed.replaced {
            " (replaced the manifest already there)"
        } else {
            ""
        }
    );
    if !converted.notes.is_empty() {
        println!(
            "\n{} note{} about this import:",
            converted.notes.len(),
            if converted.notes.len() == 1 { "" } else { "s" }
        );
        for note in &converted.notes {
            println!("  - {note}");
        }
    }
    println!("\nPick it under the gear menu on croft's next launch.");
    Ok(())
}

/// A per-run value for the scratch directory's name. Not a secret and not a
/// CSPRNG: it only has to be unpredictable enough that another user cannot
/// pre-create the path, and `create_dir` failing on collision is what
/// actually enforces exclusivity.
pub(super) fn scratch_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        ^ (&scratch_nonce as *const _ as u128)
}

/// Resolve a marketplace reference to a theme JSON on disk.
///
/// Returns the extracted file and the temporary directory holding it: the
/// caller keeps that alive until the conversion has read the file, and
/// dropping it takes the `.vsix` and everything lifted from it with it.
/// Nothing from the archive outlives this function.
pub(super) fn fetch_marketplace_theme(
    reference: &str,
    wanted: Option<&str>,
) -> Result<(PathBuf, crate::marketplace::Scratch)> {
    use crate::marketplace;
    let id = marketplace::parse_ref(reference)?;
    // A private scratch in the temp dir rather than a dev-only temp crate:
    // this is a production path. `Scratch` owns it and removes it on drop,
    // so every failure below cleans up as reliably as success does.
    //
    // The name carries randomness, the directory is created with `create_dir`
    // so an existing path is an ERROR rather than something to delete, and it
    // is mode 0o700 so a permissive umask cannot leave it writable by another
    // local user — the writes here follow symlinks, unlike `extract_member`.
    let scratch = marketplace::Scratch::new(std::env::temp_dir().join(format!(
        "croft-theme-import-{}-{:x}",
        std::process::id(),
        scratch_nonce()
    )))?;
    let dir = scratch.path();
    eprintln!(
        "Downloading {}.{} from the marketplace…",
        id.publisher, id.name
    );
    let vsix = marketplace::download_vsix(&id, dir)?;
    let pkg = marketplace::read_member(&vsix, "extension/package.json", dir)?;
    // The NLS bundle is optional — most extensions have none — so a missing
    // or unreadable one leaves the labels as written rather than failing.
    let nls = marketplace::read_member(&vsix, "extension/package.nls.json", dir).ok();
    let themes = marketplace::contributed_themes(&pkg, nls.as_deref())?;
    let picked = marketplace::pick_theme(&themes, wanted)?;
    eprintln!("  taking \"{}\" ({})", picked.label, picked.path);
    let entry = marketplace::extract_theme_chain(&vsix, &picked.path, dir)?;
    Ok((entry, scratch))
}
