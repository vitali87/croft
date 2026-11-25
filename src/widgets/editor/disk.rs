//! Opening, reloading and saving a file: reading and decoding it, line endings and
//! encodings, outside changes and conflicts, write protection, and the cleanups on save.

use super::*;

/// Hard cap on the size of a single text file the editor will load.
/// 50MB comfortably accommodates real-world LSP / build / debug logs
/// (the user hit "File too large" on a 7.4MB lsp.log under the old 5MB
/// cap) while still capping pathological inputs that would freeze the
/// UI thread during initial `Vec<String>` allocation + tree-sitter
/// highlight recompute. The cap applies AFTER the dedicated image /
/// PDF / spreadsheet branches in `open`, so larger media keep their
/// own per-format limits.
pub(super) const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;

/// Outcome of an FS-sync sweep over a single tab (`reload_or_flag_conflict`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalChange {
    /// Disk matches the buffer's last-synced stamp; nothing to do.
    Unchanged,
    /// The buffer was clean and has been reloaded from disk.
    Reloaded,
    /// Disk changed but the reload failed (e.g. the file became unreadable).
    ReloadFailed,
    /// The buffer had unsaved edits, so the external change was flagged as a
    /// conflict instead of being applied.
    Conflict,
    /// The file is gone from disk (#900). The buffer, dirty or not, stays
    /// as it is and the next explicit save puts the file back.
    Deleted,
}

/// What the FS sweep last reported about a tab whose file stopped matching
/// it (#900): a failed reload or a deletion is announced once per change on
/// disk, not on every sweep while disk stays put.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DiskReport {
    /// The tab's `disk_stamp` when reported. A reload, save or reopen moves
    /// that on, which retires the report.
    synced: Option<(SystemTime, u64)>,
    /// What disk held then: `None` when it could not be read.
    found: Option<(SystemTime, u64)>,
    /// Whether the file was gone, not merely unreadable.
    deleted: bool,
}

/// Why a buffer is written. Only an auto-save, which fires mid-typing,
/// leaves the caret lines' trailing whitespace for later (#1455).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SaveReason {
    Explicit,
    Auto,
}

/// Outcome of a guarded `save_to_disk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    /// The buffer was written to disk.
    Saved,
    /// The file changed on disk since we last synced; the save was refused
    /// to avoid clobbering the external edit. The caller must force it.
    DiskConflict,
    /// The buffer holds characters the chosen encoding cannot represent, so
    /// writing would replace them with HTML numeric character references and
    /// lose the originals irreversibly (the buffer itself is unchanged, so
    /// undo cannot bring them back). The save was refused; the caller
    /// surfaces it and sets `lossy_save_armed` to write anyway.
    EncodingLoss,
    /// The file is the user's own but has its write bit cleared (`chmod
    /// 444`, #1379). The save was refused rather than replace a file its
    /// owner protected; the caller asks and sets `overwrite_protected_armed`
    /// to write anyway, with the file's mode put back afterwards.
    WriteProtected,
}

/// What a save of a scratch tab (#951) says instead of writing.
pub const SCRATCH_SAVE_REFUSAL: &str = "This tab isn't a file: use File: Save As… to keep a copy";

/// Why a file refuses this user's writes (#1379).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteProtection {
    /// The user owns it and has cleared its write bit: they may write it
    /// once they say so, and croft restores the mode after.
    Owned,
    /// Someone else's file the user may not write (root's `/etc/hosts`):
    /// only an elevated write can save it.
    NotOwned,
}

/// Whether the file at `path` refuses this user's writes (#1379). A file
/// the user owns is judged by its owner write bit, the way vim and VS Code
/// do, so a `chmod 444` still reads as protected for root; anyone else's
/// file by whether the kernel would let this process write it. A missing
/// file is not protected: saving creates it.
pub fn write_protection(path: &Path) -> Option<WriteProtection> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() == unsafe { libc::geteuid() } {
            return (meta.mode() & 0o200 == 0).then_some(WriteProtection::Owned);
        }
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
        (unsafe { libc::access(c.as_ptr(), libc::W_OK) } != 0).then_some(WriteProtection::NotOwned)
    }
    #[cfg(not(unix))]
    {
        meta.permissions()
            .readonly()
            .then_some(WriteProtection::Owned)
    }
}

/// Whether a failed save failed for want of permission (#1379), which an
/// elevated write can get past where no retry would.
pub fn is_permission_denied(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
    })
}

/// Per-tab results of an FS-sync sweep across all open tabs
/// (`EditorTabs::reload_externally_changed_tabs`).
#[derive(Debug, Default)]
pub struct ExternalReloadReport {
    /// Paths of clean tabs that were silently reloaded from disk.
    pub reloaded: Vec<PathBuf>,
    /// Paths of dirty tabs flagged as conflicts (not reloaded, not saved).
    pub conflicts: Vec<PathBuf>,
    /// Paths whose disk copy changed but whose reload FAILED (the file
    /// became unreadable, was replaced by a directory, a PDF's re-render
    /// died). The tab keeps its last good view; dropping these on the
    /// floor left the user staring at stale content with no indication
    /// the sweep even tried (#37).
    pub failed: Vec<PathBuf>,
    /// Paths of tabs whose file was deleted on disk (#900). The tab keeps
    /// its buffer and is marked `(deleted)` until a save puts the file back.
    pub deleted: Vec<PathBuf>,
}

impl ExternalReloadReport {
    pub fn is_empty(&self) -> bool {
        self.reloaded.is_empty()
            && self.conflicts.is_empty()
            && self.failed.is_empty()
            && self.deleted.is_empty()
    }
}

/// The line-ending style of the open buffer, shown in the status bar and
/// applied when saving. Detected on open (CRLF if any `\r\n` is present);
/// the user can switch it from the status bar, converting on the next save.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LineEnding {
    #[default]
    Lf,
    Crlf,
}

impl LineEnding {
    pub fn label(self) -> &'static str {
        match self {
            Self::Lf => "LF",
            Self::Crlf => "CRLF",
        }
    }

    pub fn sequence(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }

    /// The ending most of `text`'s line breaks use, as VS Code picks it:
    /// CRLF when more than half of the breaks hold a `\r`. A lone `\r` is a
    /// break too, and counts toward CRLF. Any `\r\n` used to mark the file
    /// CRLF, so one line pasted from Windows into an LF file made the next
    /// save rewrite every line of it (#936).
    pub fn detect(text: &str) -> Self {
        let bytes = text.as_bytes();
        let (mut breaks, mut crs) = (0usize, 0usize);
        for (i, b) in bytes.iter().enumerate() {
            match b {
                b'\n' => breaks += 1,
                b'\r' => {
                    crs += 1;
                    if bytes.get(i + 1) != Some(&b'\n') {
                        breaks += 1;
                    }
                }
                _ => {}
            }
        }
        if crs * 2 > breaks {
            Self::Crlf
        } else {
            Self::Lf
        }
    }
}

impl Editor {
    /// Open `path` in this tab. Moving the tab to another file keeps the
    /// history of the one it leaves for when that file opens again, and the
    /// file it arrives at gets its own back if it has one (#1568).
    pub fn open(&mut self, path: &Path) -> Result<()> {
        let changed_file = self.path.as_deref() != Some(path);
        if changed_file {
            self.stash_history();
        }
        let opened = self.open_file(path);
        if opened.is_ok() {
            // What the sweep said about the file this tab held is not about
            // this one, even with the same stamp (#900).
            self.disk_report = None;
            // A real file now, not a scratch tab's label (#951): kept, the
            // flag refused its saves and let it close without asking.
            self.scratch = false;
        }
        if changed_file {
            // On a failed open the tab may still hold the file it left, and
            // takes its history back the same way.
            self.restore_history();
        }
        opened
    }

    pub(super) fn open_file(&mut self, path: &Path) -> Result<()> {
        // A FIFO, socket or device blocks the first read until some other
        // process writes to it, freezing the UI thread indefinitely; none of
        // them is a document. (Directories and missing paths go on to the
        // handling they always had.)
        // Process substitution (`croft <(git show HEAD:f)`) hands over a
        // `/dev/fd/N` pipe whose writer is already running, so it reads.
        let substituted = path.starts_with("/dev/fd") || path.starts_with("/proc/self/fd");
        if let Ok(meta) = std::fs::metadata(path)
            && !meta.is_file()
            && !meta.is_dir()
            && !substituted
        {
            anyhow::bail!("{} is not a regular file", path.display());
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        // The Reopen-as-Text override is PER TAB and per file: a path
        // change is a different document, which routes normally again.
        if self.path.as_deref() != Some(path) {
            self.force_text = false;
            self.force_log = false;
        }
        // Why a `.sarif` file is showing as text, when it is: the text path
        // below reports it instead of a bare "Opened".
        let mut sarif_refusal = None;
        if !self.force_text {
            // SARIF results viewer (#577). A log that does not load (bad
            // JSON, another SARIF version) opens as text with the reason.
            if crate::sarif::view::extension_is_sarif(ext) {
                match self.open_sarif(path) {
                    Ok(()) => return Ok(()),
                    Err(e) => sarif_refusal = Some(e.to_string()),
                }
            }
            // Rendered ANSI log (#257): a colour-bearing log paints through
            // the theme palette instead of showing raw escapes. Sniffed, so a
            // plain .txt with pytest output routes here too; a failure falls
            // through to the normal text path.
            if (self.force_log || Self::should_open_as_log(path)) && self.open_log(path).is_ok() {
                return Ok(());
            }
            // Media (#183): header-probed info card; junk with a media
            // extension falls through to the binary path.
            if crate::media::extension_is_media(ext) && self.open_media_preview(path).is_ok() {
                return Ok(());
            }
            // SQLite (#182): by extension here, by magic below.
            let is_sqlite_ext = path
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(crate::sqlite_view::extension_is_sqlite);
            if is_sqlite_ext {
                // Propagate errors when the MAGIC confirms sqlite (#201
                // review: a corrupt database must error, not fall to
                // hex); a .db that is not sqlite at all falls through.
                let magic = read_file_head(path)
                    .map(|h| crate::magic::sniff(&h) == Some(crate::magic::Magic::Sqlite))
                    .unwrap_or(false);
                if magic {
                    return self.open_sqlite(path).map_err(|e| anyhow::anyhow!("{e}"));
                }
            }
            // docx/odt (#181): a document that fails the walk falls
            // through to content routing and, ultimately, hex.
            if crate::docx::extension_is_doc(ext) {
                if self.open_doc_preview(path).is_ok() {
                    return Ok(());
                }
                // A SIZE refusal must say so here (#506). Unlike the archive
                // branch below, this path has no second route to explain
                // itself: `archive::kind_from_ext` does not claim .docx or
                // .odt, so an over-cap document reaches hex with nothing
                // said unless the reason is recorded now.
                if crate::docx::is_past_listing_cap(path) {
                    self.route_note = Some((
                        path.to_path_buf(),
                        format!(
                            "document too large to list ({} bytes)",
                            std::fs::metadata(path).map(|m| m.len()).unwrap_or_default()
                        ),
                    ));
                }
            }
            // A corrupt archive falls through to content routing and,
            // ultimately, the hex fallback.
            if let Some(kind) = crate::archive::kind_from_ext(path) {
                match self.open_archive(path, kind) {
                    Ok(()) => return Ok(()),
                    // A SIZE refusal is a deliberate cap, not a parse
                    // failure (#493): the file still opens as hex, but the
                    // reason rides along so the viewer can say why the
                    // browser it expected was declined.
                    Err(e) if refusal(&e) == Some(Refusal::PastCap) => {
                        self.route_note = Some((path.to_path_buf(), e.to_string()));
                    }
                    Err(_) => {}
                }
            }
            if extension_is_image(ext) {
                // A DECODE failure means the extension lied (#174): fall
                // through to content routing instead of failing the open.
                // Filesystem errors stay errors — the file may not exist.
                match self.open_image(path) {
                    Err(e) if refusal(&e) == Some(Refusal::NotItsFormat) => {}
                    other => return other,
                }
            }
            // SVG renders through resvg into the image pipeline (#175); a
            // parse failure falls through to the XML source in the text
            // editor.
            if ext.eq_ignore_ascii_case("svg") && self.open_svg(path).is_ok() {
                return Ok(());
            }
            if extension_is_pdf(ext) && self.pdf_viewer_enabled {
                return self.open_pdf(path);
            }
            if crate::sheet::extension_is_sheet(ext) && self.csv_viewer_enabled {
                // Unsaved grid edits survive a same-path re-open (#177):
                // a tree re-click must not rebuild the view over them.
                // The FS sweep never reloads a dirty tab, and Revert
                // clears the flag first, re-arming the reload.
                if self.path.as_deref() == Some(path)
                    && self
                        .sheet
                        .as_ref()
                        .is_some_and(|v| v.dirty || v.editing.is_some())
                {
                    return Ok(());
                }
                // Same fall-through for a workbook/CSV that does not parse
                // as its extension claims (#174) — but ONLY for parse
                // failures. A SIZE refusal ("too large") is a deliberate
                // cap, and rerouting it into the text path would silently
                // bypass the guard the cap exists to enforce (#188 review).
                match self.open_sheet(path) {
                    Err(e) if refusal(&e) == Some(Refusal::NotItsFormat) => {}
                    other => return other,
                }
            }
        }
        let meta = std::fs::metadata(path)?;
        if meta.len() > MAX_FILE_BYTES {
            // Sniff only the head: the hex viewer reads windows on
            // demand and never loads the file whole, so an over-limit
            // BINARY file opens fine (#172). Only text — which the
            // editor genuinely must hold in memory — keeps the guard.
            let head = read_file_head(path)?;
            // A gzip is judged by what it decompresses to, not by its own
            // size (#1469): a 60 MiB rotated log can hold far less than
            // the decompressed-size cap.
            if !self.force_text
                && crate::magic::sniff(&head) == Some(crate::magic::Magic::Gzip)
                && self.route_gzip(path)
            {
                return Ok(());
            }
            let head_bom_text = encoding_rs::Encoding::for_bom(&head).is_some();
            if !head_bom_text && is_binary(&head) {
                return self.open_hex(path);
            }
            anyhow::bail!("File too large ({} bytes)", meta.len());
        }
        let bytes = std::fs::read(path)?;
        // Content routing (#174): the extension gave no answer, or lied
        // and fell through above. Try the sniffed kind's viewer; any
        // failure falls back to the text/binary path below, whose hex
        // fallback guarantees the open never dead-ends. The
        // `!extension_*` guards keep a route that already failed by
        // extension from being retried against the same bytes.
        match (!self.force_text)
            .then(|| crate::magic::sniff(&bytes))
            .flatten()
        {
            Some(m) if m.is_image() && !extension_is_image(ext) => {
                if self.open_image(path).is_ok() {
                    return Ok(());
                }
            }
            Some(crate::magic::Magic::Pdf) if !extension_is_pdf(ext) && self.pdf_viewer_enabled => {
                if self.open_pdf(path).is_ok() {
                    return Ok(());
                }
            }
            // A zip container is an xlsx candidate (the only zip-backed
            // format croft renders today); a plain archive fails the
            // parse and falls through. Only the `.xlsx` extension is
            // excluded — that exact route already ran and failed above;
            // a workbook wearing `.csv` (or any other sheet extension)
            // failed a DIFFERENT parser, so the xlsx retry is genuinely
            // new information (#188 review).
            Some(crate::magic::Magic::Zip) => {
                if !ext.eq_ignore_ascii_case("xlsx")
                    && self.csv_viewer_enabled
                    && let Ok(view) =
                        crate::sheet::open_sheet_with_kind(path, crate::sheet::SheetKind::Xlsx)
                {
                    self.install_sheet_view(path, view);
                    return Ok(());
                }
                // Not a workbook: browse it as the archive it is (#179).
                match self.open_archive(path, crate::archive::ArchiveKind::Zip) {
                    Ok(()) => return Ok(()),
                    // The same deliberate cap as the extension route (#493).
                    Err(e) if refusal(&e) == Some(Refusal::PastCap) => {
                        self.route_note = Some((path.to_path_buf(), e.to_string()));
                    }
                    Err(_) => {}
                }
            }
            // The remaining container kinds browse too when they parse.
            // Same shape as the zip arm above: a SIZE refusal is policy,
            // so its reason rides to the hex fallback rather than vanishing.
            Some(crate::magic::Magic::Gzip) => {
                if self.route_gzip(path) {
                    return Ok(());
                }
            }
            // A `.tar.xz`, `.tar.bz2` or `.tar.zst` under another name
            // (#1645); a plain compressed file is not a tar and falls through
            // to hex.
            Some(
                m @ (crate::magic::Magic::Xz
                | crate::magic::Magic::Bzip2
                | crate::magic::Magic::Zstd),
            ) => {
                let kind = match m {
                    crate::magic::Magic::Xz => crate::archive::ArchiveKind::TarXz,
                    crate::magic::Magic::Bzip2 => crate::archive::ArchiveKind::TarBz2,
                    _ => crate::archive::ArchiveKind::TarZst,
                };
                match self.open_archive(path, kind) {
                    Ok(()) => return Ok(()),
                    Err(e) if refusal(&e) == Some(Refusal::PastCap) => {
                        self.route_note = Some((path.to_path_buf(), e.to_string()));
                    }
                    Err(_) => {}
                }
            }
            Some(crate::magic::Magic::Sqlite) => {
                return self.open_sqlite(path);
            }
            Some(crate::magic::Magic::Tar) => {
                match self.open_archive(path, crate::archive::ArchiveKind::Tar) {
                    Ok(()) => return Ok(()),
                    Err(e) if refusal(&e) == Some(Refusal::PastCap) => {
                        self.route_note = Some((path.to_path_buf(), e.to_string()));
                    }
                    Err(_) => {}
                }
            }
            _ => {}
        }
        self.load_text(path, &bytes, sarif_refusal)
    }

    /// A file that does not exist yet, in a folder that does: an empty text
    /// buffer bound to `path`, written on the first save, as `code new.md`
    /// and `vim new.md` start one (#1157). An existing file opens as usual.
    pub fn open_new_file(&mut self, path: &Path) -> Result<()> {
        if std::fs::symlink_metadata(path).is_ok() {
            return self.open(path);
        }
        let dir = path.parent().unwrap_or(Path::new(""));
        if !dir.is_dir() {
            anyhow::bail!("no such directory {}", dir.display());
        }
        if self.path.as_deref() != Some(path) {
            self.force_text = false;
        }
        self.load_text(path, &[], None)?;
        self.disk_expect_absent = self.disk_stamp.is_none();
        self.status = format!("New file {}: saving creates it", path.display());
        Ok(())
    }

    /// Load `bytes`, the contents of `path`, as the buffer's text: the end
    /// of [`Self::open`] once no viewer claimed the file.
    pub(super) fn load_text(
        &mut self,
        path: &Path,
        bytes: &[u8],
        sarif_refusal: Option<String>,
    ) -> Result<()> {
        // Decode with the file's BOM-declared encoding if it has one, else
        // the one the user last picked for it, the `.editorconfig` charset or
        // the `files_encoding` setting (#1432), else UTF-8. A later "Reopen
        // with Encoding" overrides this.
        let sniffed = encoding_rs::Encoding::for_bom(bytes);
        let editorconfig = crate::editorconfig::for_file(path);
        let declared = sniffed
            .is_none()
            .then(|| crate::file_encoding::declared_for(path, &editorconfig))
            .flatten();
        let enc = sniffed
            .map(|(e, _)| e)
            .or(declared.map(|c| c.encoding))
            .unwrap_or(encoding_rs::UTF_8);
        // The BOM sniff comes FIRST: UTF-16 text is half NUL bytes, so the
        // binary heuristic would reject every UTF-16 file — including the ones
        // croft itself writes when the user picks UTF-16 from the encoding
        // menu. A byte-order mark is a positive declaration of text, so it
        // settles the question the heuristic is only guessing at.
        let bom_declares_text = enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE;
        if !bom_declares_text && is_binary(bytes) {
            // Never a dead-end: binary files open in the hex viewer
            // instead of failing with "Binary file" (#172).
            return self.open_hex(path);
        }
        let changed_file = self.path.as_deref() != Some(path);
        // `open` is also the same-path reload behind the FS-sync sweep and
        // every revert, so auto-detection here would throw away an encoding
        // the user picked through "Reopen with Encoding" — decoding the same
        // bytes as UTF-8 and handing the next save the mojibake to write back.
        // The choice sticks until the file itself declares otherwise.
        let enc = if changed_file || sniffed.is_some() {
            enc
        } else {
            self.encoding
        };
        self.encoding = enc;
        // `decode` strips the BOM, so remember it or the next save drops it.
        // A `utf-8-bom` charset asks for one on a file that has none yet.
        self.bom =
            sniffed.is_some() || (enc == encoding_rs::UTF_8 && declared.is_some_and(|c| c.bom));
        let (decoded, _, decode_lossy) = enc.decode(bytes);
        let text = decoded.into_owned();
        self.decode_lossy = decode_lossy;
        self.final_newline = text.ends_with(['\n', '\r']);
        // Detect the file's line-ending style before normalisation so a save
        // preserves it (and the status bar reports it).
        self.eol = LineEnding::detect(&text);
        self.lines = split_into_lines(&text);
        // The map described the text that was just replaced (#349). Clearing
        // is the safe direction: an unknown line is correct, a line credited
        // to whoever wrote the file's PREVIOUS contents is not.
        self.provenance = crate::provenance::Provenance::new();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        // VS Code's editor.detectIndentation: the freshly loaded content
        // decides the buffer's indent style unless the user overrides it.
        self.detected_indent = detect_indentation(&self.lines);
        // `.editorconfig` is read here, before the sniffed EOL above can be
        // relied on: a project that states `end_of_line` means it, and a
        // file whose current content disagrees is exactly the case the
        // property exists to fix.
        self.editorconfig = editorconfig;
        if let Some(eol) = self.editorconfig.eol {
            self.eol = match eol {
                crate::editorconfig::Eol::Lf => LineEnding::Lf,
                crate::editorconfig::Eol::Crlf => LineEnding::Crlf,
            };
        }
        self.hscroll_content_cols = None;
        self.wrap_total_cache.clear();
        self.scroll_sub = 0;
        self.path = Some(path.to_path_buf());
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_expect_absent = false;
        self.disk_conflict = false;
        // A whole-buffer swap: whatever the previous contents could not be
        // encoded as is no longer this tab's problem.
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        // Say up front that the file can't be written, not at the first
        // save after the work is done (#1379).
        self.write_protected = write_protection(path).is_some();
        self.overwrite_protected_armed = false;
        self.lang = crate::highlight::lang_for_path(path);
        self.language_by_hand = false;
        self.wrap_override = None;
        // Folds are line numbers into the OLD file. A preview tab is reused for
        // the next single-click, so without this the incoming file arrives with
        // the previous one's blocks collapsed. Only for a DIFFERENT file
        // though: `open` is also the same-path reload behind every FS-sync
        // sweep, and dropping the set there popped the user's blocks open
        // whenever anything rewrote the file. That case is already covered by
        // the render-time `fold_epoch_lines` guard, which drops folds if the
        // reload changed the line count.
        if changed_file {
            self.folded.clear();
            self.hidden_ranges.clear();
            self.fold_epoch_lines = 0;
            self.lsp_folds = None;
            self.lsp_folds_lines = 0;
            self.fallback_kind_folds.clear();
            self.fallback_folds_seq = None;
            // Inline-value trailers are line numbers into the OLD file too
            // (#136 review): a reused preview tab must not dress the incoming
            // file in the previous one's debug state. The same-path reload
            // keeps them — the next stop rebuilds them anyway.
            self.inline_values.clear();
            // A symbol tab reused for another file is an ordinary tab of
            // that file: its clip and mirror state described the old one.
            self.leave_symbol_view();
            // Live Run answered for the old file's text, same as above.
            self.live_run = None;
            self.code_lenses.clear();
            self.ghost = None;
        } else if !self.folded.is_empty() {
            // The retained headers were measured against text that has just
            // been replaced. Re-measure their spans, or a reload that keeps the
            // line count while moving the indentation leaves the cache hiding
            // lines the folds no longer cover — the epoch guard below only
            // catches a change in line COUNT.
            self.rebuild_hidden_ranges();
        }
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit_kind = None;
        // A same-path reload (the FS-sync sweep after another pane saves the
        // file, an agent or formatter rewriting it, a revert) keeps the
        // rendered Markdown view (#1605): dropping it turned the tab back
        // into source whenever anything rewrote the file. The `edit_seq`
        // bump below rebuilds it from the new text. A notebook's view is
        // rebuilt below anyway, and a document's or media card's belonged to
        // a viewer this text route has just replaced.
        let keep_preview = !changed_file
            && self.lang == Some(LangKind::Markdown)
            && self
                .markdown_preview
                .as_ref()
                .is_some_and(|md| !md.notebook && md.doc_path.is_none());
        let kept_preview = if keep_preview {
            self.markdown_preview.take()
        } else {
            None
        };
        // A real file supersedes every other view, a diff included: without
        // this a restore-then-reload keeps rendering the stale diff.
        self.clear_viewers();
        self.markdown_preview = kept_preview;
        self.status = match sarif_refusal {
            Some(why) => format!("Opened {} as text: {why}", path.display()),
            None => format!("Opened {}", path.display()),
        };
        // The buffer now matches disk; bump the edit seq so the LSP doc sync
        // sees the new content (an external reload lands here too, and without
        // the bump the server keeps analysing the old text and never sends
        // fresh semantic tokens — codeberg issue #39).
        self.edit_seq = self.edit_seq.wrapping_add(1);
        // A same-path reload diffs the marks across the new text; opening a
        // different file only clamps its marks and seeds its shadow.
        self.sync_bookmarks_to_buffer();
        // A path CHANGE detaches the buffer from any collab doc it was
        // synced to: the old generation belongs to another file, and a
        // reused tab (the preview tab) keeping it would let the next collab
        // tick broadcast this file's disk text as edits to that doc. A
        // same-path reload keeps its attachment deliberately — that is how
        // an owner's reload-diff (external change, Replace All) converges
        // into the session as ops.
        if changed_file {
            self.collab_doc_gen = 0;
            self.collab_synced_seq = self.edit_seq;
        }
        // Drop any semantic-token batch measured against the previous content:
        // decoding it over the new lines paints the old file's colors at the
        // wrong offsets. Tree-sitter colors cover the gap until the fresh
        // batch triggered by the seq bump arrives.
        self.semantic_data = Vec::new();
        self.semantic_overlay = Vec::new();
        self.semantic_is_full = false;
        // Same for inlay hints: anchors measured against another file's text
        // must never splice into this one.
        self.inlay_hints = Vec::new();
        self.inlay_path = None;
        self.doc_links = Vec::new();
        self.doc_links_path = None;
        self.color_infos = Vec::new();
        self.color_path = None;
        self.inlay_spans = Vec::new();
        self.recompute_highlights();
        // Notebooks auto-open rendered (#180): the JSON stays one
        // Reopen-as-Text away, and force_text keeps that choice sticky.
        if !self.force_text && path.extension().and_then(|e| e.to_str()) == Some("ipynb") {
            self.build_notebook_preview();
        }
        Ok(())
    }

    /// Whether this buffer's text is exactly what `bytes` decode to (#349),
    /// for checking a stored snapshot against the buffer the map would land
    /// on. Compared as LINES, the way `open` builds them: a file's trailing
    /// newline is not a line of its own, so comparing raw bytes would call
    /// every ordinary text file a mismatch.
    pub fn holds_exactly(&self, bytes: &[u8]) -> bool {
        let enc = encoding_rs::Encoding::for_bom(bytes)
            .map(|(e, _)| e)
            .unwrap_or(self.encoding);
        // Built exactly as `open` builds `lines` from the same bytes: the
        // replacement-character text of an undecodable byte on both sides
        // (`open` discards `had_errors` too), and the empty file's one
        // sentinel line. Any divergence here reads as "not this text" and
        // silently withholds a map that should have come back.
        let (text, _, _) = enc.decode(bytes);
        let mut want = split_into_lines(&text);
        if want.is_empty() {
            want.push(String::new());
        }
        want == self.lines
    }

    /// The bytes this buffer would write to disk, for binding a recorded
    /// provenance map to the text it describes (#349). `None` when the
    /// encoding cannot represent the text, since a save would refuse it too.
    pub fn bytes_for_disk(&self) -> Option<Vec<u8>> {
        let content = self.content_for_disk();
        let (encoded, had_errors) = self.encode_for_disk(&content);
        (!had_errors).then_some(encoded)
    }

    /// The map a save of this tab records beside its history snapshot
    /// (#349): the buffer's own for text, empty for a viewer, whose
    /// placeholder line no seat wrote. Read off the tab that was saved, never
    /// looked up by path: two tabs on one file are two buffers with two maps.
    pub fn provenance_to_record(&self) -> crate::provenance::Provenance {
        if self.has_non_text_view() {
            return crate::provenance::Provenance::new();
        }
        self.provenance.clone()
    }

    /// The file's (mtime, length) when this tab last matched disk; `None` for
    /// a buffer with no file behind it.
    pub fn disk_stamp(&self) -> Option<(SystemTime, u64)> {
        self.disk_stamp
    }

    /// Put `text` in the buffer as unsaved edits over the file it was opened
    /// from: the copy hot exit or the update relaunch kept of them (#862).
    /// A whole-buffer swap, so everything measured against the opened text
    /// is redone as an edit redoes it: syntax spans, bracket colours and the
    /// string and comment ranges they skip, and the edit counter the LSP,
    /// git gutter, fold tables and previews resync on. Folds and server fold
    /// spans were line numbers into the old text and go; so does the undo
    /// history, which would put the file's text back under the restored
    /// edits, and the authorship map, which described the file's lines.
    pub fn restore_unsaved_text(&mut self, text: &str) {
        self.lines = text.split('\n').map(str::to_string).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.selection = None;
        self.provenance = crate::provenance::Provenance::new();
        self.folded.clear();
        self.hidden_ranges.clear();
        self.fold_epoch_lines = 0;
        self.lsp_folds = None;
        self.lsp_folds_lines = 0;
        // Dirty, stamped as edited now (auto save's delay keys on it), and
        // the edit counter moves.
        self.mark_buffer_changed();
        // Nothing was edited at a place in this session to go back to.
        self.last_edit_pos = None;
        self.recompute_highlights();
    }

    /// Anchor the disk stamp at `stamp`, the file as this buffer's edits
    /// last matched it, for a buffer rebuilt from a copy of its unsaved text
    /// (hot exit, #862): a file that changed since then reads as changed
    /// externally, so a save asks before overwriting that change.
    pub fn set_disk_stamp(&mut self, stamp: Option<(SystemTime, u64)>) {
        self.disk_stamp = stamp;
    }

    /// Re-read the open file and decode it with `enc` (status-bar "Reopen with
    /// Encoding"). Replaces the buffer, resets the cursor, and re-highlights;
    /// subsequent saves re-encode with `enc`. No-op without a backing file.
    pub fn reopen_with_encoding(&mut self, enc: &'static encoding_rs::Encoding) -> Result<()> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No file open"))?;
        let bytes = std::fs::read(&path)?;
        // `decode` does its own BOM sniffing and OVERRIDES the encoding passed
        // in when the file carries one, returning what it actually used. Take
        // that, or the buffer would hold text decoded one way while claiming to
        // be another — and the save would then re-encode it wrongly.
        let (decoded, used, decode_lossy) = enc.decode(&bytes);
        let text = decoded.into_owned();
        self.encoding = used;
        // Keep the choice for the next time this file opens (#1432).
        crate::file_encoding::remember(&path, used);
        self.decode_lossy = decode_lossy;
        // Re-sniff against the bytes just read: reinterpreting the file under a
        // new encoding must not carry the previous one's BOM answer over.
        self.bom = encoding_rs::Encoding::for_bom(&bytes).is_some();
        self.final_newline = text.ends_with(['\n', '\r']);
        self.eol = LineEnding::detect(&text);
        self.lines = split_into_lines(&text);
        // The map described the text that was just replaced (#349). Clearing
        // is the safe direction: an unknown line is correct, a line credited
        // to whoever wrote the file's PREVIOUS contents is not.
        self.provenance = crate::provenance::Provenance::new();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        // A whole-buffer swap invalidates both history stacks: a redo/undo
        // popped afterwards would reinstate the buffer as decoded under the
        // OLD encoding, silently discarding the re-decode.
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.hscroll_content_cols = None;
        self.wrap_total_cache.clear();
        self.scroll = 0;
        self.scroll_sub = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.selection = None;
        // The buffer now matches disk decoded as `enc`, so it is clean; bump the
        // edit seq so the LSP/outline/highlight resync sees the new content.
        self.dirty = false;
        // The encoding just changed, so a refusal recorded against the old
        // one is stale — without this, auto save would keep skipping a tab
        // whose new encoding covers the buffer fine.
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        // The git-gutter baseline was decoded with the old encoding: drop it
        // so the next sweep refetches HEAD decoded the new way (#1242).
        self.git_baseline_for = None;
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.sync_bookmarks_to_buffer();
        self.recompute_highlights();
        Ok(())
    }

    /// Returns true if the on-disk file at `event_path` is the file currently
    /// open in this editor (used by the filesystem watcher to decide whether
    /// to reload).
    pub fn matches_open_path(&self, event_path: &Path) -> bool {
        let Some(open) = self.path.as_ref() else {
            return false;
        };
        if open == event_path {
            return true;
        }
        if let (Ok(a), Ok(b)) = (open.canonicalize(), event_path.canonicalize()) {
            return a == b;
        }
        false
    }

    /// Read the file's identity (mtime, len) for change detection. `None`
    /// when there is no path or the file can't be stat'd (e.g. it was
    /// deleted out from under us).
    pub(super) fn disk_stamp_of(path: &Path) -> Option<(SystemTime, u64)> {
        let meta = std::fs::metadata(path).ok()?;
        Some((meta.modified().ok()?, meta.len()))
    }

    /// [`Self::disk_stamp_of`], with a SQLite database's write-ahead log
    /// folded in when `sqlite` (#1643): in WAL mode a commit lands in
    /// `<db>-wal` and leaves the database file alone until a checkpoint,
    /// which can wait until the writing app exits. The log is part of what a
    /// reader sees, so a commit moves the stamp and the tab reloads. Only a
    /// SQLite view counts it: a plain file with a `<name>-wal` sibling keeps
    /// its own stamp, untouched by whatever that sibling is.
    pub(super) fn disk_stamp_with_wal(path: &Path, sqlite: bool) -> Option<(SystemTime, u64)> {
        let stamp = Self::disk_stamp_of(path)?;
        if !sqlite {
            return Some(stamp);
        }
        Some(
            match sqlite_wal_path(path).and_then(|w| std::fs::metadata(w).ok()) {
                Some(wal) => (
                    wal.modified().map_or(stamp.0, |m| m.max(stamp.0)),
                    stamp.1 + wal.len(),
                ),
                None => stamp,
            },
        )
    }

    /// This tab's stamp of `path` now: [`Self::disk_stamp_with_wal`] for
    /// whatever view the tab is.
    pub(super) fn current_disk_stamp(&self, path: &Path) -> Option<(SystemTime, u64)> {
        Self::disk_stamp_with_wal(path, self.is_sqlite_view())
    }

    /// Record that this buffer is now in sync with disk. Called at open and
    /// after a successful save so the next external write is detected.
    pub(super) fn mark_synced_with_disk(&mut self) {
        self.disk_stamp = self
            .path
            .as_deref()
            .and_then(|p| self.current_disk_stamp(p));
        self.disk_conflict = false;
        // A new buffer<->disk sync point; undo snapshots from before it
        // restore as dirty (see `restore_snapshot`).
        self.save_seq = self.save_seq.wrapping_add(1);
    }

    /// True when the file on disk no longer matches the (mtime, len) we last
    /// loaded or saved — i.e. some other process wrote it. A file with no
    /// recorded stamp (blank buffer, or never synced) is treated as
    /// unchanged so we never reload or block on a phantom diff.
    pub fn disk_changed_externally(&self) -> bool {
        let Some(path) = self.path.as_deref() else {
            return false;
        };
        match self.disk_stamp {
            Some(known) => self.current_disk_stamp(path) != Some(known),
            // A new file not written yet: another process creating it is
            // a change, or the first save would clobber what it wrote.
            None => self.disk_expect_absent && Self::disk_stamp_of(path).is_some(),
        }
    }

    /// Reload from disk *only if* there are no unsaved local edits. Returns
    /// `Some(Ok(()))` if a reload happened, `Some(Err(_))` if reload failed,
    /// `None` if reload was skipped because the buffer is dirty (caller
    /// should surface a "file changed on disk" warning instead). Retained
    /// for the explicit-revert path; the FS sync sweep uses
    /// `reload_or_flag_conflict`.
    pub fn reload_if_clean(&mut self) -> Option<Result<()>> {
        if self.dirty {
            return None;
        }
        Some(self.reload_from_disk())
    }

    /// Reload the buffer from disk, preserving the cursor/scroll position as
    /// far as the new contents allow. Updates the disk stamp so the reloaded
    /// state is the new sync point.
    pub(super) fn reload_from_disk(&mut self) -> Result<()> {
        let Some(path) = self.path.as_ref().cloned() else {
            return Ok(());
        };
        let prev_row = self.cursor_row;
        let prev_col = self.cursor_col;
        let prev_scroll = self.scroll;
        // A PDF's page is its cursor: a rebuild on disk (pdflatex finishing)
        // must not snap the reader back to page 1. `open_pdf` consumes the
        // request and comes up on that page directly — restoring it with a
        // second render after the open left a window where that render's
        // transient failure lost the reader's place (#72).
        self.pdf_restore_page = self.pdf_page().filter(|&p| p > 1);
        // A SARIF viewer keeps the reader's place, and the logs added to
        // it, across a rewrite of its log (#577).
        let old_sarif = self.sarif.take();
        // A SQLite tab keeps its table, page, cell and scroll across a
        // reload, which a WAL-mode app's every commit now triggers (#1643).
        let sqlite_place = self.sqlite_place();

        // A rendered log keeps its place too, and one viewed at its end
        // follows it, since a log grows while it is read (#1538). A place
        // an earlier reload is still settling is the one to keep: the scroll
        // was clamped to that reload's partial index, so it says nothing
        // about where the reader is, and it would drop the end marker.
        let pending_top = self.log_reload_top;
        let log_place = self.log.as_ref().map(|log| {
            let rows = (self.last_inner.height as usize).saturating_sub(1).max(1);
            let at_end = !log.indexing() && prev_scroll + rows >= log.len();
            let top = match pending_top {
                Some(top) => top,
                None if at_end => usize::MAX,
                None => prev_scroll,
            };
            (top, log.highlight(), log.selection, log.last_body)
        });
        // `open` starts a text tab's history afresh. A reload keeps it
        // (#1357): the text from before an agent's or formatter's rewrite is
        // one undo away, with everything typed before that behind it.
        let history = (!self.has_non_text_view()).then(|| {
            (
                self.snapshot(),
                std::mem::take(&mut self.undo_stack),
                std::mem::take(&mut self.redo_stack),
                self.encoding,
            )
        });
        let result = self.open(&path);
        if let Some((before, undo, redo, encoding)) = history {
            self.carry_history_across_reload(before, undo, redo, encoding, result.is_ok());
        }
        if let (Some(old), Some(view)) = (old_sarif.as_ref(), self.sarif.as_mut()) {
            view.carry_from(old);
        }
        if let (Some(place), true) = (sqlite_place, result.is_ok()) {
            self.restore_sqlite_place(&path, place);
        }
        // Cleared even when the open bailed before reaching `open_pdf`, so
        // the request can never leak into an unrelated later open.
        self.pdf_restore_page = None;
        // Clamp the restored cursor to the new contents so it stays valid
        // even if the file shrank.
        self.cursor_row = prev_row.min(self.lines.len().saturating_sub(1));
        self.cursor_col = prev_col.min(self.line_char_len(self.cursor_row));
        self.scroll = prev_scroll.min(self.lines.len().saturating_sub(1));
        if let Some((top, highlight, selection, last_body)) = log_place
            && let Some(log) = self.log.as_mut()
        {
            log.set_highlight(highlight);
            let (len, indexing) = (log.len(), log.indexing());
            log.selection = selection.filter(|&(a, b)| indexing || a.0.max(b.0) < len);
            log.last_body = last_body;
            if !indexing && self.active_search_match.is_some_and(|(row, ..)| row >= len) {
                self.active_search_match = None;
            }
            self.log_reload_top = Some(top);
            self.settle_log_reload();
        }
        result
    }

    /// Put back the history [`Self::reload_from_disk`] took aside, with the
    /// pre-reload text as one more step when the reload changed it. Dropped
    /// when the file came back as something other than text in the same
    /// encoding, where the old lines mean nothing; kept untouched when the
    /// open failed and the buffer was never replaced.
    pub(super) fn carry_history_across_reload(
        &mut self,
        before: Snapshot,
        mut undo: Vec<Snapshot>,
        redo: Vec<Snapshot>,
        encoding: &'static encoding_rs::Encoding,
        reloaded: bool,
    ) {
        if !reloaded {
            self.undo_stack = undo;
            self.redo_stack = redo;
            return;
        }
        if self.has_non_text_view() || self.encoding != encoding {
            return;
        }
        if before.lines == self.lines[..] {
            // Only the stamp moved (`touch`, a formatter with nothing to
            // do): no step that undoes to the same text.
            self.undo_stack = undo;
            self.redo_stack = redo;
        } else {
            // Its own step, never merged with typing; like any new edit it
            // ends the redo branch.
            undo.push(before);
            self.undo_stack = undo;
            self.trim_undo_stack();
            self.undo_step_id = next_undo_step_id();
            self.mirrored_step = None;
        }
        // A new buffer<->disk sync point: every kept step restores as dirty.
        self.save_seq = self.save_seq.wrapping_add(1);
    }

    /// Discard local edits and reload from disk unconditionally — the
    /// "Revert" half of a conflict resolution.
    pub fn revert_to_disk(&mut self) -> Result<()> {
        // A hex tab's pending overwrites are the thing being reverted:
        // discard them first so the same-path refresh guard in
        // `open_hex` lets the disk reload through (#173).
        if let Some(view) = self.hex.as_mut() {
            view.discard_edits();
        }
        // Same for a sheet tab's grid edits (#177): drop the dirty flag
        // so the same-path guard in `open` lets the rebuild through.
        if let Some(view) = self.sheet.as_mut() {
            view.dirty = false;
            view.editing = None;
        }
        // Reload FIRST: clearing `dirty` before a reload that then fails
        // (file deleted between the conflict popup and the Enter) would
        // launder unsaved edits as clean — the tab loses its marker and
        // the next FS sweep silently auto-reverts them away.
        self.reload_from_disk()?;
        self.dirty = false;
        Ok(())
    }

    /// File: Revert File (#1285): [`Self::revert_to_disk`] as one undo
    /// step. A reload starts a fresh history, so the history from before it
    /// is carried across with the reverted-away text on top: undo brings
    /// the edits back, marked unsaved, and redo reverts again.
    pub fn revert_as_undo_step(&mut self) -> Result<()> {
        let mut undo = std::mem::take(&mut self.undo_stack);
        undo.push(self.snapshot());
        let reverted = self.revert_to_disk();
        if reverted.is_err() {
            // Nothing was replaced: the history stays as it was.
            undo.pop();
        }
        self.undo_stack = undo;
        self.trim_undo_stack();
        self.redo_stack.clear();
        self.undo_step_id = next_undo_step_id();
        self.last_edit_kind = None;
        reverted
    }

    /// FS-sync sweep entry point, applied to every open tab. Compares the
    /// file's current disk stamp against the buffer's last-synced stamp and:
    ///   * clean buffer + external change -> silently reload (VS Code's
    ///     non-dirty auto-revert behaviour),
    ///   * dirty buffer + external change -> flag a conflict so neither the
    ///     buffer nor the disk is clobbered,
    ///   * no change -> nothing.
    ///
    /// A failed reload or a deletion is reported once, then the sweep stays
    /// quiet until disk changes again (#900).
    pub fn reload_or_flag_conflict(&mut self) -> ExternalChange {
        if !self.disk_changed_externally() {
            self.disk_report = None;
            return ExternalChange::Unchanged;
        }
        let found = self
            .path
            .as_deref()
            .and_then(|p| self.current_disk_stamp(p));
        // Only a file that is not there is deleted: a stat that fails any
        // other way (an I/O error, a parent turned unreadable) is a failed
        // reload, and its save keeps the conflict check.
        let deleted = found.is_none() && self.path.as_deref().is_some_and(gone_from_disk);
        let report = DiskReport {
            synced: self.disk_stamp,
            found,
            deleted,
        };
        if self.disk_report == Some(report) {
            if deleted || self.dirty {
                return ExternalChange::Unchanged;
            }
            // Reported once, but a clean tab keeps trying: a read can start
            // working again with nothing on disk moving, as when a
            // permission is put back.
            return match self.reload_from_disk() {
                Ok(()) => {
                    self.disk_report = None;
                    ExternalChange::Reloaded
                }
                Err(_) => {
                    self.disk_report = Some(DiskReport {
                        synced: self.disk_stamp,
                        ..report
                    });
                    ExternalChange::Unchanged
                }
            };
        }
        if deleted {
            // Gone, not changed: there is nothing to reload, and no external
            // edit for the buffer to conflict with or a save to clobber.
            self.disk_report = Some(report);
            self.disk_conflict = false;
            return ExternalChange::Deleted;
        }
        self.disk_report = None;
        if self.dirty {
            // Announce the conflict only on the transition into it. A buffer
            // that stays dirty while disk keeps differing must not re-fire on
            // every FS poll, or the confirm popup would reopen endlessly.
            let newly_conflicted = !self.disk_conflict;
            self.disk_conflict = true;
            return if newly_conflicted {
                ExternalChange::Conflict
            } else {
                ExternalChange::Unchanged
            };
        }
        match self.reload_from_disk() {
            Ok(()) => ExternalChange::Reloaded,
            Err(_) => {
                // Keyed on the stamp as the failed open left it, which is
                // what the next sweep compares.
                self.disk_report = Some(DiskReport {
                    synced: self.disk_stamp,
                    found,
                    deleted: false,
                });
                ExternalChange::ReloadFailed
            }
        }
    }

    /// True while this tab's file is known to be gone from disk (#900): the
    /// sweep reported it deleted and nothing has synced the tab since.
    pub fn deleted_on_disk(&self) -> bool {
        self.disk_report
            .is_some_and(|r| r.deleted && r.synced == self.disk_stamp)
    }

    /// Save, refusing to overwrite if the file changed on disk since we last
    /// synced (an external edit would be silently lost otherwise). Returns
    /// `DiskConflict` instead of writing in that case; the caller surfaces
    /// the conflict and may call `save_to_disk_force` to overwrite anyway.
    pub fn save_to_disk(&mut self) -> Result<SaveOutcome> {
        self.save_to_disk_for(SaveReason::Explicit)
    }

    /// `save_to_disk` for auto-save: the one save that leaves the caret
    /// lines' trailing whitespace alone, since it fires mid-typing (#1455).
    pub fn auto_save_to_disk(&mut self) -> Result<SaveOutcome> {
        self.save_to_disk_for(SaveReason::Auto)
    }

    pub(super) fn save_to_disk_for(&mut self, reason: SaveReason) -> Result<SaveOutcome> {
        if self.disk_changed_externally() {
            // A file deleted on disk holds no external edit to clobber, so an
            // explicit save puts it back, as VS Code does (#900). Auto save
            // leaves it gone: a checkout or `git stash` may have meant that.
            if self.path.as_deref().is_some_and(gone_from_disk) {
                if reason == SaveReason::Auto {
                    anyhow::bail!("Deleted on disk: only an explicit save recreates it");
                }
                return self.write_buffer_to_disk(reason);
            }
            self.disk_conflict = true;
            return Ok(SaveOutcome::DiskConflict);
        }
        self.write_buffer_to_disk(reason)
    }

    /// Save past a disk conflict, overwriting the external change. The
    /// "Overwrite" half of a conflict resolution.
    ///
    /// It does NOT bypass the encoding-loss guard: consenting to overwrite
    /// someone else's edit is not consent to mangle your own characters, so a
    /// buffer with both problems still reports `EncodingLoss` here and needs
    /// `lossy_save_armed` on top.
    pub fn save_to_disk_force(&mut self) -> Result<SaveOutcome> {
        self.write_buffer_to_disk(SaveReason::Explicit)
    }

    /// File: Save As… (#1203): write the buffer to `path` and make this tab
    /// that file, with the grammar its name calls for. The tab stays on its
    /// own path, untouched, unless the bytes land. A scratch tab (#951)
    /// becomes that file too.
    pub fn save_as(&mut self, path: &Path) -> Result<SaveOutcome> {
        let kept = (
            self.path.replace(path.to_path_buf()),
            self.disk_stamp,
            self.disk_conflict,
            std::mem::take(&mut self.scratch),
        );
        self.disk_stamp = Self::disk_stamp_of(path);
        self.disk_conflict = false;
        let outcome = self.write_buffer_to_disk(SaveReason::Explicit);
        if matches!(outcome, Ok(SaveOutcome::Saved)) {
            // Named now: its `Untitled-n` is free for the next one (#1580).
            self.untitled = None;
            let lang = crate::highlight::lang_for_path(path);
            if lang != self.lang {
                self.set_language(lang);
            }
        } else {
            (self.path, self.disk_stamp, self.disk_conflict, self.scratch) = kept;
        }
        outcome
    }

    /// Encode the buffer for disk in the encoding it claims, re-emitting the
    /// byte-order mark if the file had one.
    ///
    /// UTF-16 is hand-rolled because `encoding_rs` deliberately cannot produce
    /// it: the WHATWG Encoding Standard makes UTF-16 decode-only, so
    /// `Encoding::encode` routes UTF-16LE/BE (and REPLACEMENT) through
    /// `output_encoding()` to UTF-8 and hands back the string's UTF-8 bytes.
    /// Taking that output at face value wrote a file that disagreed with the
    /// encoding the status bar reported for it.
    ///
    /// Returns the bytes and whether the encoder had to substitute: for a
    /// legacy encoding `encoding_rs` does NOT write `?` the way iconv-lite
    /// (and so VS Code) does — per the WHATWG Encoding Standard it emits an
    /// HTML numeric character reference, which is valid content that no
    /// longer round-trips. Callers must not write that without consent.
    pub(super) fn encode_for_disk(&self, content: &str) -> (Vec<u8>, bool) {
        let enc = self.encoding;
        if enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE {
            let le = enc == encoding_rs::UTF_16LE;
            let mut out = Vec::with_capacity(content.len() * 2 + 2);
            // ALWAYS, regardless of whether the source had one: UTF-16 is
            // otherwise indistinguishable from binary, so a file written
            // without it cannot be reopened by croft or auto-detected by
            // anything else. VS Code's UTF-16 entries write it unconditionally
            // for the same reason.
            out.extend_from_slice(if le { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] });
            for unit in content.encode_utf16() {
                out.extend_from_slice(&if le {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            // Every `char` encodes to UTF-16 losslessly (a `str` cannot hold
            // an unpaired surrogate), so this branch never substitutes.
            return (out, false);
        }
        let (encoded, _, had_errors) = enc.encode(content);
        if self.bom && enc == encoding_rs::UTF_8 {
            let mut out = Vec::with_capacity(encoded.len() + 3);
            out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            out.extend_from_slice(&encoded);
            return (out, had_errors);
        }
        (encoded.into_owned(), had_errors)
    }

    /// The distinct characters `encoding` cannot represent, in first-seen
    /// order and capped, for the message an `EncodingLoss` refusal shows.
    ///
    /// Walks the buffer and encodes each non-ASCII character on its own, so
    /// call it once to build that message rather than per frame. Empty for
    /// UTF-8 and UTF-16, which cover every `char`.
    pub fn unmappable_chars(&self) -> Vec<char> {
        /// Enough to name the problem without turning the status line into a
        /// character dump.
        const MAX: usize = 8;
        let enc = self.encoding;
        if enc == encoding_rs::UTF_8 || enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE
        {
            return Vec::new();
        }
        let mut out: Vec<char> = Vec::new();
        let mut buf = [0u8; 4];
        for line in &self.lines {
            // ASCII is mappable in every encoding croft offers, and is the
            // overwhelming majority of a source file: skip it without paying
            // for an encode call.
            for ch in line.chars().filter(|c| !c.is_ascii()) {
                if out.contains(&ch) {
                    continue;
                }
                let (_, _, had_errors) = enc.encode(ch.encode_utf8(&mut buf));
                if had_errors {
                    out.push(ch);
                    if out.len() == MAX {
                        return out;
                    }
                }
            }
        }
        out
    }

    /// The buffer as `\n`-joined text with the final newline the file has:
    /// what language servers are sent (#911), and an external formatter
    /// that reads the whole file (#1255). Sent without it, every formatter
    /// added a newline the save then wrote a second one after.
    pub fn document_text(&self) -> String {
        let mut text = self.lines.join("\n");
        if self.sends_final_newline() {
            text.push('\n');
        }
        text
    }

    /// Whether [`Self::document_text`] ends in a newline: the file has one
    /// and the buffer is not empty, which a save writes as nothing.
    pub(super) fn sends_final_newline(&self) -> bool {
        self.final_newline
            && (self.lines.len() > 1 || self.lines.first().is_some_and(|l| !l.is_empty()))
    }

    /// Whether the buffer's last line ends in a line break on disk.
    pub fn has_final_newline(&self) -> bool {
        self.final_newline
    }

    /// The text a save writes: the lines joined with the file's EOL, and
    /// the final newline the file had. An emptied buffer writes nothing.
    pub(super) fn content_for_disk(&self) -> String {
        let mut content = self.lines.join(self.eol.sequence());
        if self.final_newline && !content.is_empty() {
            content.push_str(self.eol.sequence());
        }
        content
    }

    pub(super) fn write_buffer_to_disk(&mut self, reason: SaveReason) -> Result<SaveOutcome> {
        // Preview tabs (image/PDF, sheet, diff, hex) hold the whole-
        // buffer-swap PLACEHOLDER in `lines`, not the file's content:
        // serialising it truncated the previewed file to nothing (#185).
        // Guarded here — the declared single choke point — so no save
        // path (explicit, force, auto, format-on-save) can route around.
        if self.has_non_text_view() {
            anyhow::bail!("This tab is a read-only preview; nothing to save");
        }
        // Same choke point for a scratch tab (#951): its path is a label.
        if self.scratch {
            anyhow::bail!(SCRATCH_SAVE_REFUSAL);
        }
        self.apply_editorconfig_on_save(reason);
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No file open"))?
            .clone();
        let content = self.content_for_disk();
        let (encoded, had_errors) = self.encode_for_disk(&content);
        // The single choke point every save funnels through, so no caller can
        // route around the guard. The consent flag survives a failed write
        // (the user still consented) and is cleared only once the bytes land.
        // The decode's replacement characters are only a loss while they are
        // still in the text: a buffer rewritten past them saves freely.
        let writes_replacements = self.decode_lossy && content.contains('\u{fffd}');
        if (had_errors || writes_replacements) && !self.lossy_save_armed {
            self.encoding_loss = true;
            return Ok(SaveOutcome::EncodingLoss);
        }
        // A file its owner write-protected is not replaced without asking
        // (#1379): the rename below would otherwise go straight past the
        // missing write bit. Someone else's file goes on to fail with the
        // kernel's own "Permission denied", which the caller turns into the
        // offer of an elevated save.
        let protection = write_protection(&path);
        self.write_protected = protection.is_some();
        let lift = protection == Some(WriteProtection::Owned);
        if lift && !self.overwrite_protected_armed {
            return Ok(SaveOutcome::WriteProtected);
        }
        // The folder can go while the tab is open (a `git checkout` of a
        // branch without it, #1203). Put it back, as VS Code does, rather
        // than strand the edits; a path that can't be a folder still fails.
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let written = if lift {
            Self::write_lifting_protection(&path, &encoded)
        } else {
            crate::prefs::replace_file_contents(&path, &encoded)
        };
        if let Err(e) = written {
            // Only a write that got as far as changing the file makes what
            // is on disk croft's own doing: a stale stamp then made the sync
            // sweep offer to "reload" it over the only good copy. An
            // untouched file keeps its stamp, so an edit made outside croft
            // is still noticed and not overwritten by the next save.
            if e.touched {
                self.disk_stamp = Self::disk_stamp_of(&path);
            }
            return Err(e.error.into());
        }
        self.decode_lossy = false;
        self.encoding_loss = false;
        self.lossy_save_armed = false;
        self.overwrite_protected_armed = false;
        self.dirty = false;
        // The next keystroke opens a fresh undo step: coalescing across the
        // save point would make one Cmd+Z discard work from both sides.
        self.last_edit_kind = None;
        self.status = format!("Saved {}", path.display());
        // The buffer now matches disk, so this is the new sync point and any
        // prior conflict is resolved.
        self.mark_synced_with_disk();
        Ok(SaveOutcome::Saved)
    }

    /// Overwrite the user's own write-protected file (#1379): give it the
    /// owner write bit for the length of the write, then put its mode back,
    /// whether or not the write landed, so the file stays protected.
    pub(super) fn write_lifting_protection(
        path: &Path,
        bytes: &[u8],
    ) -> std::result::Result<(), crate::prefs::ReplaceError> {
        let untouched = |error| crate::prefs::ReplaceError {
            error,
            touched: false,
        };
        let target = std::fs::canonicalize(path).map_err(untouched)?;
        let mode = std::fs::metadata(&target).map_err(untouched)?.permissions();
        let mut writable = mode.clone();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            writable.set_mode(mode.mode() | 0o200);
        }
        #[cfg(not(unix))]
        #[allow(clippy::permissions_set_readonly_false)]
        writable.set_readonly(false);
        std::fs::set_permissions(&target, writable).map_err(untouched)?;
        let written = crate::prefs::replace_file_contents(&target, bytes);
        let restored = std::fs::set_permissions(&target, mode);
        written?;
        restored.map_err(|error| crate::prefs::ReplaceError {
            error,
            touched: true,
        })
    }

    /// The bytes a save would write now, with the save-time `.editorconfig`
    /// fixes applied to the buffer first, as a save applies them. None when
    /// writing them would lose characters (see `SaveOutcome::EncodingLoss`).
    pub fn prepare_bytes_for_save(&mut self) -> Option<Vec<u8>> {
        self.apply_editorconfig_on_save(SaveReason::Explicit);
        let content = self.content_for_disk();
        let (encoded, had_errors) = self.encode_for_disk(&content);
        let writes_replacements = self.decode_lossy && content.contains('\u{fffd}');
        (!had_errors && !writes_replacements).then_some(encoded)
    }

    /// Settle a save another process made on this tab's behalf (#1379's
    /// `sudo tee`): when the file now holds exactly `written`, the buffer is
    /// in sync with it, and clean unless it was edited since. Returns false,
    /// changing nothing, while the file holds anything else.
    pub fn settle_save_made_elsewhere(&mut self, written: &[u8]) -> bool {
        let Some(path) = self.path.clone() else {
            return false;
        };
        if std::fs::read(&path).ok().as_deref() != Some(written) {
            return false;
        }
        self.set_dirty(self.bytes_for_disk().as_deref() != Some(written));
        self.write_protected = write_protection(&path).is_some();
        // A consent to overwrite was for a save this one has now made.
        self.overwrite_protected_armed = false;
        self.last_edit_kind = None;
        self.mark_synced_with_disk();
        true
    }

    /// `trim_trailing_whitespace` for a save. An auto-save spares every line
    /// a caret sits on (primary and extra carets): it runs a second after
    /// typing stops, so trimming the caret's own line would eat the space
    /// just typed and glue the next word onto the previous one. The caret
    /// lines are normalised by a later save once the caret has moved. Any
    /// other save trims them too, as VS Code does (#1455): the line just
    /// edited is the one with the stray spaces. Unlike the command, it keeps
    /// the extra carets, clamped to their trimmed lines.
    pub(super) fn trim_trailing_whitespace_on_save(&mut self, reason: SaveReason) -> bool {
        let spared: std::collections::HashSet<usize> = match reason {
            SaveReason::Auto => std::iter::once(self.cursor_row)
                .chain(self.carets.iter().map(|c| c.head.0))
                .collect(),
            SaveReason::Explicit => std::collections::HashSet::new(),
        };
        let dirty = |(i, l): (usize, &String)| {
            !spared.contains(&i) && l.trim_end_matches([' ', '\t']).len() != l.len()
        };
        if !self.lines.iter().enumerate().any(dirty) {
            return false;
        }
        self.push_undo(EditKind::TrimWhitespace);
        for (i, line) in self.lines.iter_mut().enumerate() {
            if !spared.contains(&i) {
                let trimmed_len = line.trim_end_matches([' ', '\t']).len();
                line.truncate(trimmed_len);
            }
        }
        self.remap_carets(|p| p);
        self.mark_buffer_changed();
        self.recompute_highlights();
        true
    }

    /// Apply the clean-ups that act at save time, from the single write
    /// choke point so every save path (explicit, force, auto,
    /// format-on-save) gets them. Only an auto-save spares the caret lines
    /// from the trim.
    ///
    /// Each is a no-op unless asked for: by a `.editorconfig` property, or
    /// by the global setting (#1657), off by default as in VS Code.
    /// Rewriting someone's whitespace unasked would be a surprise, and a
    /// noisy diff. A `.editorconfig` value wins over the global one for its
    /// file, either way, as it does for indentation.
    pub(super) fn apply_editorconfig_on_save(&mut self, reason: SaveReason) -> bool {
        let mut changed = false;
        let trim_whitespace = self
            .editorconfig
            .trim_trailing_whitespace
            .unwrap_or(self.save_cleanup.trim_trailing_whitespace);
        let final_newline = self
            .editorconfig
            .insert_final_newline
            .unwrap_or(self.save_cleanup.insert_final_newline);
        if trim_whitespace {
            changed |= self.trim_trailing_whitespace_on_save(reason);
        }
        if self.save_cleanup.trim_final_newlines {
            changed |= self.trim_final_newlines_on_save();
        }
        if final_newline {
            // `lines` joins with the EOL on write, so a trailing empty
            // element IS the final newline. An empty buffer is left alone:
            // a zero-byte file has no last line to terminate.
            let empty_buffer = self.lines.len() == 1 && self.lines[0].is_empty();
            if !empty_buffer
                && !self.final_newline
                && !self.lines.last().is_some_and(String::is_empty)
            {
                self.push_undo(EditKind::InsertFinalNewline);
                self.lines.push(String::new());
                self.mark_buffer_changed();
                self.recompute_highlights();
                changed = true;
            }
        }
        changed
    }

    /// `files.trimFinalNewlines` for a save (#1657): the empty lines after
    /// the last line of text go, but the newline that ends that line stays.
    /// With `final_newline` set it is written from the flag; without, it is
    /// the one trailing empty element, so that one is kept. No line a
    /// caret sits on goes, on any save: auto-save runs a second after
    /// typing stops, and must not eat the line just opened with Enter. The command (`trim_final_newlines`) drops them all, as
    /// it always has.
    pub(super) fn trim_final_newlines_on_save(&mut self) -> bool {
        let mut text_end = self.lines.len();
        while text_end > 1 && self.lines[text_end - 1].is_empty() {
            text_end -= 1;
        }
        let ends_line = usize::from(!self.final_newline && text_end < self.lines.len());
        let last_caret = std::iter::once(self.cursor_row)
            .chain(self.carets.iter().map(|c| c.head.0))
            .max()
            .unwrap_or(0);
        let keep = (text_end + ends_line).max(last_caret + 1);
        if keep >= self.lines.len() {
            return false;
        }
        self.push_undo(EditKind::TrimFinalNewlines);
        self.lines.truncate(keep);
        self.mark_buffer_changed();
        self.recompute_highlights();
        true
    }

    /// The `.editorconfig` properties in force for this buffer.
    pub fn editorconfig_props(&self) -> crate::editorconfig::Props {
        self.editorconfig
    }
}

/// The global on-save clean-ups (#1657): VS Code's `files.insertFinalNewline`,
/// `files.trimTrailingWhitespace` and `files.trimFinalNewlines`, all off by
/// default as they are there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SaveCleanup {
    pub insert_final_newline: bool,
    pub trim_trailing_whitespace: bool,
    pub trim_final_newlines: bool,
}

impl SaveCleanup {
    pub(crate) fn from_prefs(p: &crate::prefs::Prefs) -> Self {
        Self {
            insert_final_newline: p.insert_final_newline,
            trim_trailing_whitespace: p.trim_trailing_whitespace,
            trim_final_newlines: p.trim_final_newlines,
        }
    }
}

/// First 4 KiB of a file (or all of it when shorter): the sample both the
/// BOM sniff and [`is_binary`] need, read without loading the whole file
/// so the over-limit routing in `open` stays O(1) in file size.
pub(super) fn read_file_head(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut head = vec![0u8; 4096];
    let mut f = std::fs::File::open(path)?;
    let mut filled = 0;
    while filled < head.len() {
        let n = f.read(&mut head[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    head.truncate(filled);
    Ok(head)
}

/// Decode a file's bytes (a git blob or the working copy) the way its tab
/// decodes them, for a diff or gutter baseline against that tab (#1242). A
/// byte-order mark wins, as on open. Under UTF-8 only valid UTF-8 decodes,
/// as before; under an encoding the user picked, everything but binary does.
pub fn decode_blob(bytes: &[u8], enc: &'static encoding_rs::Encoding) -> Option<String> {
    if let Some((bom_enc, _)) = encoding_rs::Encoding::for_bom(bytes) {
        return Some(bom_enc.decode(bytes).0.into_owned());
    }
    if enc == encoding_rs::UTF_8 {
        return String::from_utf8(bytes.to_vec()).ok();
    }
    if is_binary(bytes) {
        return None;
    }
    Some(enc.decode(bytes).0.into_owned())
}

pub(super) fn is_binary(data: &[u8]) -> bool {
    let sample = &data[..data.len().min(4096)];
    if sample.contains(&0) {
        return true;
    }
    if sample.is_empty() {
        return false;
    }
    let nontext = sample
        .iter()
        .filter(|&&b| !(b >= 0x20 || matches!(b, b'\n' | b'\r' | b'\t' | 0x0c | 0x08)))
        .count();
    (nontext as f32 / sample.len() as f32) > 0.30
}

/// Fold every line-ending convention to `\n` so callers can split on it
/// alone. Shared by the file loader and the find bar's replacement paths: a
/// replacement carrying `\r` (VS Code's escape, or pasted CRLF text) must
/// become a real line break, never a bare CR sitting inside a line — a CRLF
/// buffer would then save `\r\r\n` and every line count would disagree.
pub(crate) fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}
