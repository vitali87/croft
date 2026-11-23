//! `croft setup-cross`: the targets the local cross-build for `croft <host>` builds for, and
//! installing zig, cargo-zigbuild and those targets.

use super::*;

/// The triples `croft <host>` cross-builds for. A rustup target belongs to ONE
/// toolchain, so these must be re-added whenever `rust-toolchain.toml` moves;
/// `remote::tests::the_pinned_toolchain_has_every_cross_target` is the guard.
pub(crate) const CROSS_TARGETS: &[&str] =
    &["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"];

pub(super) fn setup_cross(yes: bool) -> Result<()> {
    println!(
        "This installs the local-cross-build fast path used by `croft <host>`:\n  - homebrew package: zig (the linker)\n  - cargo subcommand: cargo-zigbuild\n  - rustup targets: {}",
        CROSS_TARGETS.join(", ")
    );
    println!(
        "After this finishes, every remote connect rsyncs a pre-built static binary instead of recompiling croft on the box."
    );
    if !yes {
        print!("Apply this change? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }
    install_zig_if_missing()?;
    install_cargo_zigbuild_if_missing()?;
    for triple in CROSS_TARGETS {
        install_rust_target_if_missing(triple)?;
    }
    println!("Cross-compile fast path ready. Next `croft <host>` will ship a pre-built binary.");
    Ok(())
}

pub(super) fn install_zig_if_missing() -> Result<()> {
    if std::process::Command::new("zig")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        println!("zig is already installed.");
        return Ok(());
    }
    if std::process::Command::new("brew")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        println!("Installing zig via Homebrew");
        let status = std::process::Command::new("brew")
            .args(["install", "zig"])
            .status()
            .context("running brew install zig")?;
        if !status.success() {
            anyhow::bail!("brew install zig exited with {status}");
        }
    } else {
        anyhow::bail!(
            "zig is not installed and Homebrew is not available; install zig manually (https://ziglang.org/download/) and rerun this command"
        );
    }
    Ok(())
}

pub(super) fn install_cargo_zigbuild_if_missing() -> Result<()> {
    if std::process::Command::new("cargo-zigbuild")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        println!("cargo-zigbuild is already installed.");
        return Ok(());
    }
    println!("Installing cargo-zigbuild via cargo");
    let status = std::process::Command::new("cargo")
        .args(["install", "cargo-zigbuild", "--locked"])
        .status()
        .context("running cargo install cargo-zigbuild")?;
    if !status.success() {
        anyhow::bail!("cargo install cargo-zigbuild exited with {status}");
    }
    Ok(())
}

/// Add `triple` to the toolchain the cross-build actually uses.
///
/// Both rustup calls run FROM THE CHECKOUT so `rust-toolchain.toml` resolves
/// the pinned channel. Run from anywhere else, rustup answers for the default
/// toolchain: `setup-cross` would then print "already installed" for a target
/// that belongs to another channel entirely and exit successfully, having
/// fixed nothing, while `croft <host>` kept compiling on the remote box.
pub(super) fn install_rust_target_if_missing(triple: &str) -> Result<()> {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"));
    let installed = std::process::Command::new("rustup")
        .args(["target", "list", "--installed"])
        .current_dir(checkout)
        .output()
        .context("rustup target list --installed")?;
    if !installed.status.success() {
        anyhow::bail!("rustup not available; install Rust via https://rustup.rs and rerun");
    }
    if String::from_utf8_lossy(&installed.stdout)
        .lines()
        .any(|line| line.trim() == triple)
    {
        println!("rustup target {triple} is already installed.");
        return Ok(());
    }
    println!("Adding rustup target {triple}");
    let status = std::process::Command::new("rustup")
        .args(["target", "add", triple])
        .current_dir(checkout)
        .status()
        .context("rustup target add")?;
    if !status.success() {
        anyhow::bail!("rustup target add {triple} exited with {status}");
    }
    Ok(())
}
