//! Word 97-2003 `.doc` read-only preview (#1979).
//!
//! A `.doc` is an OLE compound file holding Word's binary format, not the zip
//! of XML a `.docx` is, so it needs a reader of its own. This one follows
//! [MS-DOC]'s "Retrieving Text": the FIB at the head of the `WordDocument`
//! stream names the table stream (`0Table` or `1Table`) and the piece table in
//! it, whose pieces map character positions to the bytes holding them, each
//! piece either cp1252 ("compressed") or UTF-16LE.
//!
//! Structure comes from the paragraph properties. The bin table points at
//! 512-byte PAPX pages, which give each paragraph mark its style (the style
//! sheet says which styles are the built-in headings) and the sprms saying
//! whether it is a table cell, the end of a table row, or a list item. The
//! result is the markdown the docx walker emits, so the preview renders through
//! the same builder. Character formatting, pictures, headers, footnotes and
//! comments are not read: the fidelity is the body text and its structure.

use std::io::Read as _;
use std::path::Path;

/// The compound-file signature every `.doc` starts with.
const CFB_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
/// `wIdent`, the first two bytes of a Word document's FIB.
const WORD_IDENT: u16 = 0xA5EC;
/// The `nFib` Word 97 writes, and every later version in the FIB base. Word 6
/// and Word 95 wrote 101 to 104, with a FIB this reader does not know.
const NFIB_WORD97: u16 = 0x00C0;
const F_ENCRYPTED: u16 = 0x0100;
const F_WHICH_TABLE: u16 = 0x0200;
/// Indexes into the FIB's fc/lcb pairs (`FibRgFcLcb97`).
const FC_STSHF: usize = 1;
const FC_PLCF_BTE_PAPX: usize = 13;
const FC_CLX: usize = 33;

const SPRM_P_ISTD: u16 = 0x4600;
const SPRM_P_F_IN_TABLE: u16 = 0x2416;
const SPRM_P_F_TTP: u16 = 0x2417;
const SPRM_P_ILFO: u16 = 0x460B;
const SPRM_P_CHG_TABS: u16 = 0xC615;
const SPRM_T_DEF_TABLE: u16 = 0xD608;
/// A piece's single-sprm modifier (Prm0) names its sprm by index; these two
/// are the ones that change what this reader shows.
const ISPRM_P_F_IN_TABLE: u16 = 0x18;
const ISPRM_P_F_TTP: u16 = 0x19;

const CELL_END: char = '\u{07}';
const LINE_BREAK: char = '\u{0B}';
const PAGE_BREAK: char = '\u{0C}';
const PARAGRAPH_END: char = '\r';
const FIELD_BEGIN: char = '\u{13}';
const FIELD_SEPARATOR: char = '\u{14}';
const FIELD_END: char = '\u{15}';
const NON_BREAKING_HYPHEN: char = '\u{1E}';
/// Stands in for a line break while a paragraph is read. It is whitespace to
/// `trim`, so a break at either end of a paragraph goes with the spaces there.
const BREAK_MARK: char = '\u{2028}';

/// Whether `head` starts like a compound file, the container a `.doc` is.
pub fn is_compound_file(head: &[u8]) -> bool {
    head.starts_with(&CFB_MAGIC)
}

/// The document's body as markdown, or `None` when the file is not a Word
/// document. A Word document croft cannot read, being password-protected or
/// from Word 6 or 95, comes back as a one-line note saying so, which tells the
/// user more than a hex dump of it would.
pub fn to_markdown(path: &Path) -> Option<String> {
    if std::fs::metadata(path).ok()?.len() > crate::docx::MAX_DOC_BYTES {
        return None;
    }
    let mut file = cfb::open(path).ok()?;
    let word = read_stream(&mut file, "/WordDocument")?;
    let fib = match Fib::parse(&word)? {
        Ok(fib) => fib,
        Err(Unreadable::Encrypted) => {
            return Some(String::from(
                "*This document is password-protected, so its text can't be shown.*\n",
            ));
        }
        Err(Unreadable::BeforeWord97) => {
            return Some(String::from(
                "*This document is from Word 6 or Word 95. Documents from Word 97 \
                 and later open here.*\n",
            ));
        }
    };
    let table = read_stream(&mut file, fib.table_stream)?;
    let (pieces, grpprls) = pieces(&table, fib.clx)?;
    let runs = paragraph_runs(&word, &table, fib.bte_papx);
    let headings = heading_levels(&table, fib.stshf);
    let mut out = Markdown::new(&runs, &grpprls, &headings);
    main_text(&word, &pieces, fib.ccp_text, |c, at, prm| {
        out.push(c, at, prm)
    });
    Some(out.finish())
}

fn read_stream(file: &mut cfb::CompoundFile<std::fs::File>, name: &str) -> Option<Vec<u8>> {
    let stream = file.open_stream(name).ok()?;
    let mut bytes = Vec::new();
    stream
        .take(crate::docx::MAX_DOC_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= crate::docx::MAX_DOC_BYTES).then_some(bytes)
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(i..i.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(i..i.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// `len` bytes of `b` from `at`, both as stored in the file.
fn slice_at(b: &[u8], at: u32, len: u32) -> Option<&[u8]> {
    let at = at as usize;
    b.get(at..at.checked_add(len as usize)?)
}

/// Why a Word document has no text this reader can show.
enum Unreadable {
    Encrypted,
    BeforeWord97,
}

/// The parts of the FIB this reader uses.
struct Fib {
    table_stream: &'static str,
    /// How many characters the main document has; footnotes, headers and the
    /// rest follow it in the same text.
    ccp_text: u32,
    stshf: (u32, u32),
    bte_papx: (u32, u32),
    clx: (u32, u32),
}

impl Fib {
    /// `None` when `word` is not a Word document's stream.
    fn parse(word: &[u8]) -> Option<Result<Fib, Unreadable>> {
        if u16_at(word, 0)? != WORD_IDENT {
            return None;
        }
        if u16_at(word, 2)? < NFIB_WORD97 {
            return Some(Err(Unreadable::BeforeWord97));
        }
        let flags = u16_at(word, 0x0A)?;
        if flags & F_ENCRYPTED != 0 {
            return Some(Err(Unreadable::Encrypted));
        }
        // The 32-byte base, then three arrays each led by its length: 16-bit
        // words, 32-bit words, then the fc/lcb pairs.
        let csw = u16_at(word, 32)? as usize;
        let cslw_at = 34 + 2 * csw;
        let rglw = cslw_at + 2;
        let ccp_text = u32_at(word, rglw + 4 * 3)?;
        let cb_at = rglw + 4 * u16_at(word, cslw_at)? as usize;
        if (u16_at(word, cb_at)? as usize) <= FC_CLX {
            return None;
        }
        let pair = |i: usize| {
            Some((
                u32_at(word, cb_at + 2 + 8 * i)?,
                u32_at(word, cb_at + 6 + 8 * i)?,
            ))
        };
        Some(Ok(Fib {
            table_stream: if flags & F_WHICH_TABLE != 0 {
                "/1Table"
            } else {
                "/0Table"
            },
            ccp_text,
            stshf: pair(FC_STSHF)?,
            bte_papx: pair(FC_PLCF_BTE_PAPX)?,
            clx: pair(FC_CLX)?,
        }))
    }
}

/// A run of text: character positions `cp..cp_end`, stored from byte `at` of
/// the `WordDocument` stream.
struct Piece {
    cp: u32,
    cp_end: u32,
    at: usize,
    compressed: bool,
    /// Formatting applied to everything in the piece, after a paragraph's
    /// own (a Word "fast save" records edits this way).
    prm: u16,
}

/// The piece table, from the Clx in the table stream, with the grpprls its
/// pieces' `Prm1` modifiers point at.
fn pieces(table: &[u8], (fc, lcb): (u32, u32)) -> Option<(Vec<Piece>, Vec<&[u8]>)> {
    let clx = slice_at(table, fc, lcb)?;
    let mut i = 0;
    let mut grpprls = Vec::new();
    while *clx.get(i)? == 1 {
        let cb = i16::from_le_bytes([*clx.get(i + 1)?, *clx.get(i + 2)?]);
        let cb = usize::try_from(cb).ok()?;
        grpprls.push(clx.get(i + 3..i + 3 + cb)?);
        i += 3 + cb;
    }
    if *clx.get(i)? != 2 {
        return None;
    }
    let lcb = u32_at(clx, i + 1)?;
    let plc = slice_at(clx, u32::try_from(i + 5).ok()?, lcb)?;
    let n = (lcb as usize).checked_sub(4)? / 12;
    let pieces = (0..n)
        .map(|k| {
            let pcd = 4 * (n + 1) + 8 * k;
            let raw = u32_at(plc, pcd + 2)?;
            let compressed = raw & 0x4000_0000 != 0;
            let fc = (raw & 0x3FFF_FFFF) as usize;
            Some(Piece {
                cp: u32_at(plc, 4 * k)?,
                cp_end: u32_at(plc, 4 * (k + 1))?,
                // A compressed piece's fc counts as if it were UTF-16.
                at: if compressed { fc / 2 } else { fc },
                compressed,
                prm: u16_at(plc, pcd + 6)?,
            })
        })
        .collect::<Option<_>>()?;
    Some((pieces, grpprls))
}

/// Hand `f` the main document's characters in order, each with the byte of
/// the `WordDocument` stream it is stored at, which is what the paragraph
/// properties key on, and its piece's `Prm`.
///
/// Counted against the pieces' own lengths, not their CPs, and never more
/// characters than the stream has bytes: every character takes at least one,
/// so a piece table claiming more is lying, and believing it would let a
/// few kilobytes of file ask for gigabytes of text.
fn main_text(word: &[u8], pieces: &[Piece], ccp_text: u32, mut f: impl FnMut(char, usize, u16)) {
    let mut left = (ccp_text as usize).min(word.len());
    for p in pieces {
        let n = (p.cp_end.saturating_sub(p.cp) as usize).min(left);
        if n == 0 {
            break;
        }
        left -= n;
        let width = if p.compressed { 1 } else { 2 };
        let Some(bytes) = word.get(p.at..p.at.saturating_add(n * width)) else {
            break;
        };
        if p.compressed {
            // cp1252 maps every byte to exactly one character.
            let (text, _) = encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes);
            text.chars().zip(p.at..).for_each(|(c, at)| f(c, at, p.prm));
        } else {
            let units = bytes
                .chunks_exact(2)
                .map(|u| u16::from_le_bytes([u[0], u[1]]));
            let mut at = p.at;
            for c in char::decode_utf16(units) {
                let c = c.unwrap_or(char::REPLACEMENT_CHARACTER);
                f(c, at, p.prm);
                at += 2 * c.len_utf16();
            }
        }
    }
}

/// What the PAPX of one paragraph says.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Para {
    istd: u16,
    in_table: bool,
    /// The paragraph is a table row's end mark, not a cell's.
    row_end: bool,
    listed: bool,
}

/// Paragraph properties by the bytes of text they cover, ordered by start.
fn paragraph_runs(word: &[u8], table: &[u8], (fc, lcb): (u32, u32)) -> Vec<(usize, usize, Para)> {
    let mut runs = Vec::new();
    let Some(plc) = slice_at(table, fc, lcb) else {
        return runs;
    };
    let m = (lcb as usize).saturating_sub(4) / 8;
    // Each page is read once, however many entries name it: repeats would
    // only repeat its runs, and a crafted table repeating one page millions
    // of times would turn kilobytes of file into gigabytes of runs.
    let mut seen = std::collections::HashSet::new();
    for k in 0..m {
        let Some(pn) = u32_at(plc, 4 * (m + 1) + 4 * k) else {
            break;
        };
        let pn = pn & 0x3F_FFFF;
        let at = pn as usize * 512;
        let Some(page) = word.get(at..at + 512) else {
            continue;
        };
        if !seen.insert(pn) {
            continue;
        }
        // rgfc (crun + 1 offsets) and rgbx (13 bytes a run) share the page
        // with the count in its last byte.
        let crun = page[511] as usize;
        if 4 * (crun + 1) + 13 * crun > 511 {
            continue;
        }
        for r in 0..crun {
            let (Some(start), Some(end), Some(&offset)) = (
                u32_at(page, 4 * r),
                u32_at(page, 4 * (r + 1)),
                page.get(4 * (crun + 1) + 13 * r),
            ) else {
                break;
            };
            let para = papx(page, 2 * offset as usize).unwrap_or_default();
            runs.push((start as usize, end as usize, para));
        }
    }
    runs.sort_by_key(|r| r.0);
    runs
}

/// The PAPX at byte `at` of an FKP page. Offset 0 means the paragraph has no
/// properties of its own.
fn papx(page: &[u8], at: usize) -> Option<Para> {
    if at == 0 {
        return Some(Para::default());
    }
    let cb = *page.get(at)? as usize;
    let (start, len) = match cb {
        0 => (at + 2, 2 * *page.get(at + 1)? as usize),
        cb => (at + 1, 2 * cb - 1),
    };
    let props = page.get(start..start + len)?;
    let mut para = Para {
        istd: u16_at(props, 0)?,
        ..Para::default()
    };
    para.apply(props.get(2..)?);
    Some(para)
}

impl Para {
    /// Apply the paragraph sprms in `grpprl`, later ones winning.
    fn apply(&mut self, grpprl: &[u8]) {
        for (sprm, operand) in Sprms(grpprl) {
            let set = operand.iter().any(|b| *b != 0);
            match sprm {
                SPRM_P_ISTD => self.istd = u16_at(operand, 0).unwrap_or(self.istd),
                SPRM_P_F_IN_TABLE => self.in_table = set,
                SPRM_P_F_TTP => self.row_end = set,
                SPRM_P_ILFO => self.listed = set,
                _ => {}
            }
        }
    }

    /// Apply a piece's `Prm` after the paragraph's own properties, as
    /// [MS-DOC]'s "Direct Paragraph Formatting" orders it. A `Prm0` holds
    /// one sprm by index and a one-byte operand; a `Prm1` names one of the
    /// Clx's grpprls.
    fn apply_prm(&mut self, prm: u16, grpprls: &[&[u8]]) {
        if prm & 1 == 0 {
            let set = prm >> 8 != 0;
            match (prm >> 1) & 0x7F {
                ISPRM_P_F_IN_TABLE => self.in_table = set,
                ISPRM_P_F_TTP => self.row_end = set,
                _ => {}
            }
        } else if let Some(grpprl) = grpprls.get(usize::from(prm >> 1)) {
            self.apply(grpprl);
        }
    }
}

/// The property modifiers in a grpprl, each with its operand. A sprm's top
/// three bits give its operand's size; size 6 is variable and led by a count.
struct Sprms<'a>(&'a [u8]);

impl<'a> Iterator for Sprms<'a> {
    type Item = (u16, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let sprm = u16_at(self.0, 0)?;
        let rest = self.0.get(2..)?;
        let (skip, len) = match sprm >> 13 {
            0 | 1 => (0, 1),
            2 | 4 | 5 => (0, 2),
            3 => (0, 4),
            7 => (0, 3),
            _ => match sprm {
                // A two-byte count, one more than the bytes after it.
                SPRM_T_DEF_TABLE => (2, (u16_at(rest, 0)? as usize).checked_sub(1)?),
                // A count of 255 means the operand measures itself: deleted
                // tabs with their close distances, then added tabs.
                SPRM_P_CHG_TABS if rest.first() == Some(&255) => {
                    let deleted = *rest.get(1)? as usize;
                    let added = *rest.get(2 + 4 * deleted)? as usize;
                    (1, 2 + 4 * deleted + 3 * added)
                }
                _ => (1, *rest.first()? as usize),
            },
        };
        let operand = rest.get(skip..skip + len)?;
        self.0 = &rest[skip + len..];
        Some((sprm, operand))
    }
}

/// The heading level (1 to 9, 0 for none) of each style index. A style is a
/// heading when it is one of Word's built-in "Heading 1" to "Heading 9",
/// whatever the document calls it.
fn heading_levels(table: &[u8], (fc, lcb): (u32, u32)) -> Vec<u8> {
    let mut levels = Vec::new();
    let Some(stsh) = slice_at(table, fc, lcb) else {
        return levels;
    };
    let (Some(cb_stshi), Some(cstd)) = (u16_at(stsh, 0), u16_at(stsh, 2)) else {
        return levels;
    };
    let mut at = 2 + cb_stshi as usize;
    for _ in 0..cstd {
        let Some(cb_std) = u16_at(stsh, at) else {
            break;
        };
        // An empty slot has no STD at all.
        let sti = if cb_std >= 2 {
            u16_at(stsh, at + 2).map_or(0, |w| w & 0x0FFF)
        } else {
            0
        };
        levels.push(if (1..=9).contains(&sti) { sti as u8 } else { 0 });
        at += 2 + cb_std as usize;
    }
    levels
}

/// The paragraph properties for the mark stored at byte `at`.
fn para_at(runs: &[(usize, usize, Para)], at: usize) -> Para {
    let i = runs.partition_point(|r| r.0 <= at);
    i.checked_sub(1)
        .map(|i| runs[i])
        .filter(|r| at < r.1)
        .map_or_else(Para::default, |r| r.2)
}

/// The characters, walked into markdown: paragraphs, headings, list items
/// and pipe tables, with field codes left out and their results kept.
struct Markdown<'a> {
    runs: &'a [(usize, usize, Para)],
    grpprls: &'a [&'a [u8]],
    headings: &'a [u8],
    md: String,
    /// The paragraph being read.
    text: String,
    cell: String,
    row: Vec<String>,
    rows: Vec<Vec<String>>,
    /// One entry per open field: whether its result has begun. Only a result
    /// is shown, so `PAGE`, `TOC \o` and `HYPERLINK "…"` never reach the page.
    fields: Vec<bool>,
    /// How many of `fields` are still in their code, so whether text shows
    /// is one comparison however deep the fields nest.
    in_code: usize,
}

impl<'a> Markdown<'a> {
    fn new(runs: &'a [(usize, usize, Para)], grpprls: &'a [&'a [u8]], headings: &'a [u8]) -> Self {
        Markdown {
            runs,
            grpprls,
            headings,
            md: String::new(),
            text: String::new(),
            cell: String::new(),
            row: Vec::new(),
            rows: Vec::new(),
            fields: Vec::new(),
            in_code: 0,
        }
    }

    /// Take the character stored at byte `at`, in a piece whose `Prm` is
    /// `prm`.
    fn push(&mut self, c: char, at: usize, prm: u16) {
        match c {
            FIELD_BEGIN => {
                self.fields.push(false);
                self.in_code += 1;
            }
            FIELD_SEPARATOR => {
                if let Some(shown) = self.fields.last_mut().filter(|shown| !**shown) {
                    *shown = true;
                    self.in_code -= 1;
                }
            }
            FIELD_END => {
                if self.fields.pop() == Some(false) {
                    self.in_code -= 1;
                }
            }
            PARAGRAPH_END | CELL_END | PAGE_BREAK => {
                let mut para = para_at(self.runs, at);
                para.apply_prm(prm, self.grpprls);
                self.end_paragraph(c, para);
            }
            _ if self.in_code > 0 => {}
            LINE_BREAK => self.text.push(BREAK_MARK),
            '\t' => self.text.push(' '),
            NON_BREAKING_HYPHEN => self.text.push('-'),
            // Anchors for pictures, footnote and comment marks, and the
            // optional hyphen: nothing to show in the text itself.
            c if c < ' ' => {}
            c => {
                if crate::docx::is_md_meta(c) {
                    self.text.push('\\');
                }
                self.text.push(c);
            }
        }
    }

    /// A paragraph, cell or row ended with `mark`.
    fn end_paragraph(&mut self, mark: char, para: Para) {
        let text = std::mem::take(&mut self.text);
        let line = text.trim();
        if para.in_table || mark == CELL_END {
            if !line.is_empty() {
                if !self.cell.is_empty() {
                    self.cell.push(' ');
                }
                self.cell.push_str(&line.replace(BREAK_MARK, " "));
            }
            if mark == CELL_END && para.row_end {
                self.rows.push(std::mem::take(&mut self.row));
            } else if mark == CELL_END {
                self.row.push(std::mem::take(&mut self.cell));
            }
            return;
        }
        self.flush_table();
        if line.is_empty() {
            return;
        }
        match self.headings.get(para.istd as usize).copied().unwrap_or(0) {
            0 => {
                if para.listed {
                    self.md.push_str("- ");
                }
                self.md.push_str(&line.replace(BREAK_MARK, "\\\n"));
            }
            level => {
                self.md.push_str(&"#".repeat(level.min(6) as usize));
                self.md.push(' ');
                self.md.push_str(&line.replace(BREAK_MARK, " "));
            }
        }
        self.md.push_str("\n\n");
    }

    /// Write out a table being read, including a row or cell its marks never
    /// closed.
    fn flush_table(&mut self) {
        if !self.cell.is_empty() {
            self.row.push(std::mem::take(&mut self.cell));
        }
        if !self.row.is_empty() {
            self.rows.push(std::mem::take(&mut self.row));
        }
        if !self.rows.is_empty() {
            crate::docx::push_table(&mut self.md, std::mem::take(&mut self.rows));
        }
    }

    /// The markdown, with a last paragraph whose mark never came.
    fn finish(mut self) -> String {
        self.end_paragraph(PARAGRAPH_END, Para::default());
        self.flush_table();
        self.md
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real Word 97 document, written by LibreOffice from HTML: three
    /// heading levels, a hyperlink field, a two-row table, a bulleted list,
    /// a line break, Greek and Japanese text, and markdown metacharacters.
    fn fixture() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/word97/testdata/report.doc")
    }

    /// #1979: the body comes out as the structure it has in Word.
    #[test]
    fn a_word_97_document_reads_as_markdown() {
        let md = to_markdown(&fixture()).expect("a readable .doc");
        assert_eq!(
            md,
            "# Quarterly report\n\n\
             Sales rose in every region. See the site.\n\n\
             ## Regions\n\n\
             |Region|Total|\n|---|---|\n|North|1,200|\n\n\
             ### Notes\n\n\
             Café Müller – Zürich\\\nsecond line\n\n\
             - First point\n\n\
             - Second point\n\n\
             Ελληνικά and 日本語 and a \\# hash and \\*stars\\*.\n\n"
        );
    }

    /// Write a minimal Word 97 document whose text is `pieces`, each stored
    /// compressed (cp1252) or as UTF-16. No style sheet and no paragraph
    /// pages: the reader has to manage without either.
    fn synthetic(dir: &Path, pieces: &[(&[u8], bool)], ccp_text: u32) -> std::path::PathBuf {
        let pieces: Vec<_> = pieces.iter().map(|&(b, c)| (b, c, 0)).collect();
        synthetic_with_prms(dir, &pieces, &[], ccp_text)
    }

    /// [`synthetic`], with each piece carrying a `Prm` and the Clx carrying
    /// the grpprls a `Prm1` names.
    fn synthetic_with_prms(
        dir: &Path,
        pieces: &[(&[u8], bool, u16)],
        grpprls: &[&[u8]],
        ccp_text: u32,
    ) -> std::path::PathBuf {
        let mut word = vec![0u8; 1024];
        word[0..2].copy_from_slice(&WORD_IDENT.to_le_bytes());
        word[2..4].copy_from_slice(&0x00C1u16.to_le_bytes());
        word[32..34].copy_from_slice(&14u16.to_le_bytes());
        word[62..64].copy_from_slice(&22u16.to_le_bytes());
        word[76..80].copy_from_slice(&ccp_text.to_le_bytes());
        word[152..154].copy_from_slice(&93u16.to_le_bytes());
        let (mut cps, mut pcds) = (vec![0u32], Vec::new());
        for (bytes, compressed, prm) in pieces {
            let at = word.len() as u32;
            word.extend_from_slice(bytes);
            let chars = if *compressed {
                bytes.len()
            } else {
                bytes.len() / 2
            } as u32;
            cps.push(cps.last().unwrap() + chars);
            let fc = if *compressed {
                (at * 2) | 0x4000_0000
            } else {
                at
            };
            pcds.push((fc, *prm));
        }
        let mut plc: Vec<u8> = cps.iter().flat_map(|cp| cp.to_le_bytes()).collect();
        for (fc, prm) in pcds {
            plc.extend_from_slice(&[0, 0]);
            plc.extend_from_slice(&fc.to_le_bytes());
            plc.extend_from_slice(&prm.to_le_bytes());
        }
        let mut table = Vec::new();
        for grpprl in grpprls {
            table.push(1u8);
            table.extend_from_slice(&(grpprl.len() as i16).to_le_bytes());
            table.extend_from_slice(grpprl);
        }
        table.push(2u8);
        table.extend_from_slice(&(plc.len() as u32).to_le_bytes());
        table.extend_from_slice(&plc);
        let clx = 154 + 8 * FC_CLX;
        word[clx + 4..clx + 8].copy_from_slice(&(table.len() as u32).to_le_bytes());
        let path = dir.join("synthetic.doc");
        let mut file = cfb::create(&path).unwrap();
        std::io::Write::write_all(&mut file.create_stream("/WordDocument").unwrap(), &word)
            .unwrap();
        std::io::Write::write_all(&mut file.create_stream("/0Table").unwrap(), &table).unwrap();
        file.flush().unwrap();
        path
    }

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    /// #1979: Word stores plain text one byte a character, in cp1252, and
    /// switches to UTF-16 per piece. Both read, in order, and a field shows
    /// its result rather than its code.
    #[test]
    fn compressed_and_utf16_pieces_read_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let first: &[u8] = b"Caf\xe9 costs \x805.\rPage \x13 PAGE \x14";
        let second = utf16("7\u{15} of 日本.\r");
        let total = (first.len() + second.len() / 2) as u32;
        let path = synthetic(dir.path(), &[(first, true), (&second, false)], total);
        assert_eq!(
            to_markdown(&path).unwrap(),
            "Café costs €5.\n\nPage 7 of 日本.\n\n"
        );
    }

    /// The text after the main document (footnotes, headers) is not part of
    /// the body, and a field with no result shows nothing.
    #[test]
    fn only_the_main_document_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let text: &[u8] = b"Body\x13 TC \"hidden\" \x15 end.\rFootnote text\r";
        let body = b"Body\x13 TC \"hidden\" \x15 end.\r".len() as u32;
        let path = synthetic(dir.path(), &[(text, true)], body);
        assert_eq!(to_markdown(&path).unwrap(), "Body end.\n\n");
    }

    /// #1979 review: a piece's own formatting applies after the paragraph's,
    /// as Word's fast saves record it. Here the table exists only in the
    /// pieces: a `Prm0` puts the cells' paragraphs in the table, and a `Prm1`
    /// names the grpprl that makes the last mark the row's end.
    #[test]
    fn piece_formatting_shapes_paragraphs() {
        let dir = tempfile::tempdir().unwrap();
        let in_table = (ISPRM_P_F_IN_TABLE << 1) | (1 << 8);
        let row_end: &[u8] = &[0x16, 0x24, 0x01, 0x17, 0x24, 0x01];
        let cells: &[u8] = b"A1\rA2\x07B\x07";
        let mark: &[u8] = b"\x07";
        let after: &[u8] = b"After\r";
        let total = (cells.len() + mark.len() + after.len()) as u32;
        let pieces = [(cells, true, in_table), (mark, true, 1), (after, true, 0)];
        let path = synthetic_with_prms(dir.path(), &pieces, &[row_end], total);
        assert_eq!(
            to_markdown(&path).unwrap(),
            "|A1 A2|B|\n|---|---|\n\nAfter\n\n"
        );
    }

    /// #1979 review: text shows only outside every field's code, at any
    /// depth, and checking that costs the same however deep fields nest: a
    /// hundred thousand nested fields read at once.
    #[test]
    fn nested_fields_show_only_their_results() {
        let dir = tempfile::tempdir().unwrap();
        let text: &[u8] = b"a\x13 IF \x13 PAGE \x14 2\x15 = 2 \x14yes\x15 b\r";
        let path = synthetic(dir.path(), &[(text, true)], text.len() as u32);
        assert_eq!(to_markdown(&path).unwrap(), "ayes b\n\n");

        let depth = 100_000;
        let mut deep = b"\x13\x14".repeat(depth);
        deep.extend_from_slice(&b"x".repeat(depth));
        deep.extend_from_slice(&b"\x15".repeat(depth));
        deep.push(b'\r');
        let path = synthetic(dir.path(), &[(&deep, true)], deep.len() as u32);
        let t = std::time::Instant::now();
        let md = to_markdown(&path).unwrap();
        assert_eq!(md.len(), depth + 2, "every result character shows");
        assert!(t.elapsed().as_secs() < 5, "took {:?}", t.elapsed());
    }

    /// #1979 review: a bin table naming one PAPX page many times yields that
    /// page's runs once, not once per entry.
    #[test]
    fn a_repeated_papx_page_is_read_once() {
        // Page 1: one run over bytes 0..10, with no properties.
        let mut word = vec![0u8; 1024];
        word[512..516].copy_from_slice(&0u32.to_le_bytes());
        word[516..520].copy_from_slice(&10u32.to_le_bytes());
        word[1023] = 1;
        let entries = 10_000u32;
        let mut plc: Vec<u8> = (0..=entries).flat_map(|i| i.to_le_bytes()).collect();
        plc.extend((0..entries).flat_map(|_| 1u32.to_le_bytes()));
        let runs = paragraph_runs(&word, &plc, (0, plc.len() as u32));
        assert_eq!(runs, vec![(0, 10, Para::default())]);
    }

    /// A copy of the fixture with the 16-bit FIB field at `at` rewritten.
    fn patched(
        dir: &Path,
        name: &str,
        at: usize,
        patch: impl Fn(u16) -> u16,
    ) -> std::path::PathBuf {
        use std::io::{Seek as _, Write as _};
        let path = dir.join(name);
        std::fs::copy(fixture(), &path).unwrap();
        let mut file = cfb::open_rw(&path).unwrap();
        let mut stream = file.open_stream("/WordDocument").unwrap();
        let mut word = Vec::new();
        stream.read_to_end(&mut word).unwrap();
        let value = patch(u16_at(&word, at).unwrap());
        stream.seek(std::io::SeekFrom::Start(at as u64)).unwrap();
        stream.write_all(&value.to_le_bytes()).unwrap();
        drop(stream);
        file.flush().unwrap();
        path
    }

    /// Negative (#1979): a password-protected document or one from Word 95
    /// says why it shows no text, rather than showing scrambled bytes.
    #[test]
    fn unreadable_word_documents_say_why() {
        let dir = tempfile::tempdir().unwrap();
        let locked = patched(dir.path(), "locked.doc", 0x0A, |f| f | F_ENCRYPTED);
        assert!(to_markdown(&locked).unwrap().contains("password-protected"));
        let word95 = patched(dir.path(), "word95.doc", 2, |_| 0x0068);
        assert!(to_markdown(&word95).unwrap().contains("Word 95"));
    }

    /// Negative (#1979): a truncated file, a compound file that is not a
    /// Word document, and an HTML page saved as `.doc` are not read.
    #[test]
    fn files_that_are_not_word_documents_are_declined() {
        let dir = tempfile::tempdir().unwrap();
        let cut = dir.path().join("cut.doc");
        std::fs::write(&cut, &std::fs::read(fixture()).unwrap()[..3000]).unwrap();
        assert_eq!(to_markdown(&cut), None, "truncated");
        let other = dir.path().join("other.doc");
        let mut file = cfb::create(&other).unwrap();
        std::io::Write::write_all(&mut file.create_stream("/Workbook").unwrap(), b"cells").unwrap();
        file.flush().unwrap();
        assert_eq!(to_markdown(&other), None, "no WordDocument stream");
        let html = dir.path().join("page.doc");
        std::fs::write(&html, "<html><body>Hi</body></html>").unwrap();
        assert_eq!(to_markdown(&html), None, "not a compound file");
        assert!(is_compound_file(&std::fs::read(fixture()).unwrap()));
        assert!(!is_compound_file(b"<html>"));
    }

    /// A sprm whose operand runs past the end of its grpprl ends the walk
    /// rather than reading out of bounds.
    #[test]
    fn a_truncated_grpprl_stops_the_sprm_walk() {
        let sprms: Vec<_> = Sprms(&[0x16, 0x24, 0x01, 0x0B, 0x46, 0x01]).collect();
        assert_eq!(sprms, vec![(SPRM_P_F_IN_TABLE, &[1u8][..])]);
    }
}
