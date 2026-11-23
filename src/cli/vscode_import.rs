//! `croft import-vscode`: converting a VS Code profile into croft's config, and the report
//! of what was adopted, compared, dropped or refused.

use super::*;

/// `croft import-vscode`: convert a VS Code profile into croft's config.
///
/// The report is the product, not a side effect. An import that only said
/// "done" would leave the user unable to tell a setting croft adopted from
/// one it has no equivalent for, which is the difference between "my
/// settings came across" and "my settings are gone".
pub(super) fn import_vscode(from: Option<PathBuf>, dry_run: bool) -> Result<()> {
    let (dir, reading) = match from {
        Some(dir) => {
            // A mistyped path must not read as an empty profile. Every file
            // this scans is optional, so a wrong directory produces "0
            // settings, 0 keybindings, 0 snippets" and "Nothing to import",
            // which is exactly what a real but empty profile produces. The
            // auto-detected branch already fails loudly; this one now does
            // too.
            if !dir.is_dir() {
                anyhow::bail!(
                    "{} is not a directory. --from wants a VS Code user directory \
                     (the one holding settings.json), e.g. ~/.config/Code/User",
                    dir.display()
                );
            }
            let user = crate::import_vscode::user_dir_in(&dir);
            let reading = (user != dir).then(|| format!("Reading {}", user.display()));
            (user, reading)
        }
        None => {
            let found = crate::import_vscode::discover_profiles();
            let Some((label, dir)) = found.into_iter().next() else {
                anyhow::bail!(
                    "no VS Code user directory found. Pass one with --from \
                     (e.g. --from ~/.config/Code/User)"
                );
            };
            let reading = format!("Reading {label}: {}", dir.display());
            (dir, Some(reading))
        }
    };
    let mut report = crate::import_vscode::scan_profile(&dir)?;
    // Write first, report after (#1179): a reader that stops early (`| head`)
    // closes stdout, and whether the import lands must not depend on anyone
    // reading about it. A failed write is returned before the report too,
    // since a closed stdout ends the process at its first line and the
    // failure would go unsaid.
    let applied = (!dry_run && !report.is_empty())
        .then(|| crate::import_vscode::apply(&mut report))
        .transpose()?;
    if let Some(reading) = reading {
        println!("{reading}");
    }

    println!(
        "\n{} setting{} mapped, {} keybinding{} mapped, {} snippet{}",
        report.settings.len(),
        plural(report.settings.len()),
        report.keybindings.len(),
        plural(report.keybindings.len()),
        report.snippets.len(),
        plural(report.snippets.len()),
    );
    for (key, value) in &report.settings {
        println!("  {key} = {value}");
    }
    for (key, command, when) in &report.keybindings {
        match when {
            Some(when) => println!("  {key} -> {command} (when {when})"),
            None => println!("  {key} -> {command}"),
        }
    }
    if let Some(theme) = &report.theme {
        println!(
            "\nworkbench.colorTheme {:?} converts to croft theme {:?} (from {})",
            theme.label,
            theme.converted.id,
            theme.path.display()
        );
        for note in &theme.converted.notes {
            println!("  - {note}");
        }
    }
    if !report.unmapped_settings.is_empty() {
        println!(
            "\n{} setting{} croft has no equivalent for:",
            report.unmapped_settings.len(),
            plural(report.unmapped_settings.len())
        );
        for key in &report.unmapped_settings {
            println!("  {key}");
        }
    }
    if !report.dropped_keybindings.is_empty() {
        println!(
            "\n{} keybinding{} not carried over:",
            report.dropped_keybindings.len(),
            plural(report.dropped_keybindings.len())
        );
        for row in &report.dropped_keybindings {
            println!("  {row}");
        }
    }
    for warning in &report.warnings {
        println!("  warning: {warning}");
    }

    // Extensions (#352): what VS Code has installed, plus what the current
    // directory's workspace recommends, against croft's table. Read-only;
    // installing a croft equivalent is a click in the Extensions panel.
    let mut ids = crate::vscode_extensions::installed_via_code();
    let listed = !ids.is_empty();
    if let Ok(cwd) = std::env::current_dir() {
        ids.extend(crate::vscode_extensions::workspace_recommendations(&cwd));
    }
    let compared = crate::vscode_extensions::compare(ids);
    if !compared.is_empty() {
        println!(
            "\n{} VS Code extension{} ({}):",
            compared.len(),
            plural(compared.len()),
            if listed {
                "installed, plus this workspace's recommendations"
            } else {
                "this workspace's recommendations; `code` is not on PATH"
            }
        );
        for row in &compared {
            use crate::vscode_extensions::Status;
            let what = match row.mapping {
                Some(m) if m.status == Status::Builtin => {
                    format!("built in ({}): {}", m.croft, m.note)
                }
                Some(m) if m.status == Status::Installable => {
                    format!("installable ({}): {}", m.croft, m.note)
                }
                Some(m) => format!("no equivalent: {}", m.note),
                None => String::from("no equivalent: not in croft's table"),
            };
            println!("  {} -> {what}", row.id);
        }
        println!("  Open the Extensions panel (Cmd+Shift+X) to add an installable one.");
    }

    let Some(written) = applied else {
        if dry_run {
            println!("\nDry run: nothing was written.");
        } else {
            println!("\nNothing to import.");
        }
        return Ok(());
    };
    if !report.conflicts.is_empty() {
        println!(
            "\n{} value{} croft already had, left alone:",
            report.conflicts.len(),
            plural(report.conflicts.len())
        );
        for conflict in &report.conflicts {
            println!("  {conflict}");
        }
    }
    if !report.refusals.is_empty() {
        println!(
            "\n{} file{} croft declined to write:",
            report.refusals.len(),
            plural(report.refusals.len())
        );
        for refusal in &report.refusals {
            println!("  {refusal}");
        }
    }
    if written.is_empty() {
        // "Nothing needed changing" and "croft refused to write" are
        // different facts, and saying the first when the second is true tells
        // the user their settings arrived when they did not.
        if report.refusals.is_empty() {
            println!("\nEverything was already in place; nothing changed.");
        } else {
            println!("\nNothing was written: see the refusals above.");
        }
    } else {
        println!("\nWrote:");
        for path in &written {
            println!("  {}", path.display());
        }
        println!("\nRestart croft to pick the new configuration up.");
    }
    Ok(())
}

pub(super) fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
