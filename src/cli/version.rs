//! croft's version: the plain `x.y.z` that `--version` prints, and the provenance
//! `--build-info` prints (commit and build time, and how the binary was installed when a
//! package manager owns it).

/// What `croft --version` prints: the plain crate version (#282).
pub(super) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Version plus build provenance, for `--build-info`. Two builds can share a
/// crate version — 0.1.758 was both the broken and the fixed binary in the
/// 2026-08-22 session-host incident — so provenance stays one flag away
/// rather than in everybody's face. Drift detection is unaffected either way:
/// `DriftProbe` compares the baked `CROFT_GIT_HASH_FULL` against the repo's
/// current commit and never read a version string.
pub(super) const VERBOSE_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CROFT_GIT_HASH"),
    ", built ",
    env!("CROFT_BUILD_TIME"),
    ")"
);

/// `--build-info`'s text: version, commit and build time, plus how the
/// binary was installed when a package manager owns it (#375).
pub(super) fn build_info_line(source: crate::update_check::InstallSource) -> String {
    match source {
        crate::update_check::InstallSource::Homebrew => format!(
            "{}, installed with Homebrew)",
            VERBOSE_VERSION.trim_end_matches(')')
        ),
        crate::update_check::InstallSource::SelfManaged => VERBOSE_VERSION.to_string(),
    }
}
