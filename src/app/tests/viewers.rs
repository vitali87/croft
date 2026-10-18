//! Non-text viewers: images, PDF, spreadsheets, SQLite, archives, hex, docx
//! and extension viewers.

use super::*;

#[test]
fn sqlite_pages_step_at_batch_boundaries() {
    // #201 review: PageDown at the bottom of a full batch fetches the
    // next page; PageUp at the top returns.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("big.db");
    {
        let conn = rusqlite::Connection::open(&p).unwrap();
        conn.execute_batch("CREATE TABLE t (n INTEGER);").unwrap();
        let mut ins = conn.prepare("INSERT INTO t VALUES (?1)").unwrap();
        for i in 0..700 {
            ins.execute([i]).unwrap();
        }
    }
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    let view = app.editor.sheet.as_ref().unwrap();
    assert_eq!(view.sheets[0].row_count(), 500);
    assert!(view.sheets[0].name.contains("rows 1-500, more follow"));

    app.handle_sheet_key(key(KeyCode::End, KeyModifiers::SUPER));
    app.handle_sheet_key(key(KeyCode::PageDown, KeyModifiers::NONE));
    let view = app.editor.sheet.as_ref().unwrap();
    assert_eq!(view.sheets[0].row_count(), 200, "second page loaded");
    assert!(
        view.sheets[0].name.ends_with("rows 501-700"),
        "{}",
        view.sheets[0].name
    );
    assert_eq!(view.sheets[0].cell(0, 0), "500", "first value of page 2");

    app.handle_sheet_key(key(KeyCode::Home, KeyModifiers::SUPER));
    app.handle_sheet_key(key(KeyCode::PageUp, KeyModifiers::NONE));
    let view = app.editor.sheet.as_ref().unwrap();
    assert!(
        view.sheets[0].name.contains("rows 1-500"),
        "{}",
        view.sheets[0].name
    );
}

/// A database with one table `t` of `n` rows holding 1..=n.
fn sqlite_table_of(dir: &std::path::Path, n: usize) -> std::path::PathBuf {
    let p = dir.join("big.db");
    let conn = rusqlite::Connection::open(&p).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE t (n INTEGER); WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL \
         SELECT x+1 FROM c WHERE x < {n}) INSERT INTO t SELECT x FROM c;"
    ))
    .unwrap();
    p
}

/// #1222: Ctrl+End in a SQLite table goes to the table's last row, not the
/// last row of the loaded batch, and Ctrl+Home comes back to the first.
#[test]
fn ctrl_end_in_a_sqlite_table_reaches_its_last_row() {
    let tmp = tempfile::tempdir().unwrap();
    let p = sqlite_table_of(tmp.path(), 2 * crate::sqlite_view::ROW_CAP + 7);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    app.handle_sheet_key(key(KeyCode::End, KeyModifiers::CONTROL));
    let data = &app.editor.sheet.as_ref().unwrap().sheets[0];
    assert!(data.name.ends_with("rows 1001-1007"), "{}", data.name);
    assert_eq!(data.cell(data.cur_row, data.cur_col), "1007");
    app.handle_sheet_key(key(KeyCode::Home, KeyModifiers::CONTROL));
    let data = &app.editor.sheet.as_ref().unwrap().sheets[0];
    assert!(
        data.name.contains("rows 1-500, more follow"),
        "{}",
        data.name
    );
    assert_eq!(data.cell(data.cur_row, data.cur_col), "1");
    // PageUp at the top of the last page still steps back one page.
    app.handle_sheet_key(key(KeyCode::End, KeyModifiers::CONTROL));
    let view = app.editor.sheet.as_mut().unwrap();
    assert_eq!(view.sqlite_pages[0].1, 2, "the page index follows the jump");
    (view.sheets[0].cur_row, view.sheets[0].scroll_row) = (0, 0);
    app.handle_sheet_key(key(KeyCode::PageUp, KeyModifiers::NONE));
    let data = &app.editor.sheet.as_ref().unwrap().sheets[0];
    assert!(data.name.contains("rows 501-1000"), "{}", data.name);
}

/// #1222: a batch's rows are numbered by their place in the table, so
/// the grid says where in the table you are.
#[test]
fn sqlite_rows_are_numbered_by_their_place_in_the_table() {
    let tmp = tempfile::tempdir().unwrap();
    let p = sqlite_table_of(tmp.path(), 2 * crate::sqlite_view::ROW_CAP + 7);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    app.handle_sheet_key(key(KeyCode::End, KeyModifiers::CONTROL));
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let dump: String = (0..buf.area.height)
        .map(|y| {
            let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
            row + "\n"
        })
        .collect();
    assert!(
        dump.contains(" 1007 ") && dump.contains("rows 1001–1007"),
        "{dump}"
    );
}

/// Negative (#1222): a table that fits in one batch, and a CSV, keep
/// Ctrl+End within what is loaded, numbered from 1.
#[test]
fn ctrl_end_in_a_one_batch_table_or_a_csv_is_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let p = sqlite_table_of(tmp.path(), 7);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    app.handle_sheet_key(key(KeyCode::End, KeyModifiers::CONTROL));
    let data = &app.editor.sheet.as_ref().unwrap().sheets[0];
    assert_eq!(data.name, "t: 7 rows");
    assert_eq!(data.cell(data.cur_row, data.cur_col), "7");
    let csv = tmp.path().join("s.csv");
    std::fs::write(&csv, "a,b\n1,2\n3,4\n").unwrap();
    app.editor.open(&csv).unwrap();
    app.handle_sheet_key(key(KeyCode::End, KeyModifiers::CONTROL));
    let data = &app.editor.sheet.as_ref().unwrap().sheets[0];
    assert_eq!((data.cur_row, data.cur_col), (1, 1));
    assert_eq!(data.row_base, 0);
}

#[test]
fn media_file_opens_as_an_info_card_and_save_refuses() {
    // #183: a WAV opens as the rendered info card (duration, rates), a
    // save refuses (#185 class), and junk with a media extension falls
    // through to the binary path instead of erroring.
    use std::io::Write as _;
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("tone.wav");
    let mut f = std::fs::File::create(&p).unwrap();
    let byte_rate = 16000u32;
    let data = vec![0u8; byte_rate as usize]; // 1 second
    let mut hdr = Vec::new();
    hdr.extend_from_slice(b"RIFF");
    hdr.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
    hdr.extend_from_slice(b"WAVEfmt ");
    hdr.extend_from_slice(&16u32.to_le_bytes());
    hdr.extend_from_slice(&1u16.to_le_bytes());
    hdr.extend_from_slice(&1u16.to_le_bytes());
    hdr.extend_from_slice(&8000u32.to_le_bytes());
    hdr.extend_from_slice(&byte_rate.to_le_bytes());
    hdr.extend_from_slice(&2u16.to_le_bytes());
    hdr.extend_from_slice(&16u16.to_le_bytes());
    hdr.extend_from_slice(b"data");
    hdr.extend_from_slice(&(data.len() as u32).to_le_bytes());
    f.write_all(&hdr).unwrap();
    f.write_all(&data).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    let md = app
        .editor
        .markdown_preview
        .as_ref()
        .expect("media opens as the info card");
    assert!(md.media && md.doc_path.is_some());
    let all: String = md
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert!(all.contains("tone.wav"), "{all}");
    assert!(all.contains("8000 Hz"), "{all}");
    let before = std::fs::read(&p).unwrap();
    assert!(app.editor.save_to_disk().is_err(), "info card save refuses");
    assert_eq!(std::fs::read(&p).unwrap(), before);

    // Junk wearing .mp3: never a dead-end.
    let junk = tmp.path().join("noise.mp3");
    std::fs::write(&junk, b"\x00\x01 not audio").unwrap();
    let mut e = crate::widgets::editor::Editor::new();
    e.open(&junk).unwrap();
    assert!(e.hex.is_some(), "junk media falls to hex");
}

#[test]
fn docx_opens_as_a_rendered_document() {
    // #181: a docx renders headings/emphasis through the preview
    // machinery; the text side is a stub (Reopen as Text routes the zip
    // container onward, never a mojibake buffer).
    use std::io::Write as _;
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("memo.docx");
    let f = std::fs::File::create(&p).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let o =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    z.start_file("word/document.xml", o).unwrap();
    z.write_all(
        br#"<w:document xmlns:w="x"><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Memo Title</w:t></w:r></w:p>
<w:p><w:r><w:t>memo body</w:t></w:r></w:p></w:body></w:document>"#,
    )
    .unwrap();
    z.finish().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    let md = app
        .editor
        .markdown_preview
        .as_ref()
        .expect("docx opens rendered");
    assert!(md.doc_path.is_some());
    let all: String = md
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert!(all.contains("Memo Title"), "{all}");
    assert!(all.contains("memo body"));
    // #200 review: the text side is a stub - a save must refuse at the
    // choke point (#185 class), leaving the document untouched.
    let before = std::fs::read(&p).unwrap();
    assert!(
        app.editor.save_to_disk().is_err(),
        "doc preview save refuses"
    );
    assert_eq!(std::fs::read(&p).unwrap(), before, "document intact");
}

#[test]
fn archive_tab_lists_members_and_enter_opens_an_extracted_copy() {
    // #179 end-to-end: a zip opens as the member browser, Enter on a
    // text member extracts it to scratch and opens it through the
    // normal dispatch.
    use std::io::Write as _;
    let tmp = tempfile::tempdir().unwrap();
    let zp = tmp.path().join("bundle.zip");
    let f = std::fs::File::create(&zp).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    z.start_file("notes/inner.txt", opts).unwrap();
    z.write_all(b"member text").unwrap();
    z.finish().unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&zp).unwrap();
    let view = app.editor.archive.as_ref().expect("zip opens as a browser");
    assert_eq!(view.entries.len(), 1);
    assert_eq!(view.entries[0].path, "notes/inner.txt");

    app.handle_archive_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        app.editor.archive.is_none(),
        "the member opened in the editor: {}",
        app.status
    );
    assert_eq!(app.editor.lines, vec!["member text".to_string()]);
}

#[test]
fn misnamed_zip_routes_to_the_archive_browser_by_content() {
    use std::io::Write as _;
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("data.bin");
    let f = std::fs::File::create(&p).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    z.start_file("x.txt", opts).unwrap();
    z.write_all(b"in").unwrap();
    z.finish().unwrap();
    let mut e = crate::widgets::editor::Editor::new();
    e.open(&p).unwrap();
    assert!(
        e.archive.is_some(),
        "sniffed zip lands in the browser, not hex"
    );
}

#[test]
fn markdown_preview_computes_image_anchor_frame_truth() {
    // #176: the preview render must publish each image's wrapped visual
    // row and the paragraph's text area, which the overlay driver
    // consumes.
    let tmp = tempfile::tempdir().unwrap();
    image::RgbaImage::new(100, 50)
        .save(tmp.path().join("pic.png"))
        .unwrap();
    std::fs::write(
        tmp.path().join("doc.md"),
        "# Title\n\nintro text\n\n![p](pic.png)\n\ntail",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&tmp.path().join("doc.md")).unwrap();
    assert!(app.editor.toggle_markdown_preview());
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let md = app.editor.markdown_preview.as_ref().unwrap();
    assert_eq!(md.images.len(), 1);
    assert_eq!(md.anchor_rows.len(), 1);
    assert!(
        md.anchor_rows[0] >= 3,
        "the anchor sits below the heading and intro: {}",
        md.anchor_rows[0]
    );
    assert!(md.last_area.width > 0, "frame-truth area published");
    // The reserved rows are blank in the paint: no placeholder glyph.
    let mut all = String::new();
    for y in 0..30 {
        for x in 0..100 {
            all.push_str(term.backend().buffer()[(x, y)].symbol());
        }
    }
    assert!(
        !all.contains("(pic.png)"),
        "no labelled placeholder for a local image"
    );
}

/// A minimal two-page PDF whose page 1 carries the text "go to two" under a
/// GoTo link annotation targeting page 2. Hand-assembled (correct xref
/// offsets included) so the test needs no pdflatex, only poppler to read it.
fn two_page_pdf_with_internal_link() -> Vec<u8> {
    let s1 = b"BT /F1 12 Tf 20 50 Td (go to two) Tj ET";
    let s2 = b"BT /F1 12 Tf 20 50 Td (second page) Tj ET";
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R \
           /Resources << /Font << /F1 6 0 R >> >> /Annots [7 0 R] >>"
            .to_vec(),
        [
            format!("<< /Length {} >>\nstream\n", s1.len()).into_bytes(),
            s1.to_vec(),
            b"\nendstream".to_vec(),
        ]
        .concat(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 8 0 R \
           /Resources << /Font << /F1 6 0 R >> >> >>"
            .to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        b"<< /Type /Annot /Subtype /Link /Rect [18 44 92 64] /Border [0 0 0] \
           /A << /S /GoTo /D [5 0 R /Fit] >> >>"
            .to_vec(),
        [
            format!("<< /Length {} >>\nstream\n", s2.len()).into_bytes(),
            s2.to_vec(),
            b"\nendstream".to_vec(),
        ]
        .concat(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objs.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// The user's gesture end to end: a PDF preview is on screen and a click on
/// a link flips to its target page — through the REAL mouse dispatch, the
/// real overlay geometry from a rendered frame, and real poppler extraction.
#[test]
fn clicking_an_internal_pdf_link_flips_to_its_page() {
    use crossterm::event::{MouseButton, MouseEventKind};
    if crate::pdf::detect_backend() != Some(crate::pdf::PdfBackend::PdftoppmCli)
        || crate::pdf::page_links(std::path::Path::new("/nonexistent"), 1)
            .err()
            .is_some_and(|e| e.to_string().contains("install poppler"))
    {
        eprintln!("skipping: poppler (pdftoppm + pdftohtml) not installed");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let pdf = tmp.path().join("linked.pdf");
    std::fs::write(&pdf, two_page_pdf_with_internal_link()).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // A real terminal would have reported these during startup; the overlay
    // bake (whose layout the hit-test reads) is gated on both.
    app.cell_pixel = Some((10, 20));
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::Kitty;
    app.editor.open_pinned(&pdf).unwrap();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let layout = app.overlays.editor[0]
        .layout()
        .expect("rendering a PDF tab bakes the image overlay")
        .clone();
    let image = app.editor.image.as_ref().unwrap();
    let (px_w, px_h) = (image.pixel_w, image.pixel_h);
    // The link's text run "go to two" sits around (0.22, 0.47) of the page
    // (measured: x 30..103 of 300, y 62..79 of 150 in pdftohtml space).
    // Find the screen cell covering that point instead of hardcoding one,
    // so the test survives font-metric drift across poppler versions.
    let (mut col, mut row) = (0u16, 0u16);
    'outer: for r in 0..layout.cell_h {
        for c in 0..layout.cell_w {
            if let Some((x0, y0, x1, y1)) = crate::iterm2_inline::cell_source_fraction(
                (c, r),
                (10, 20),
                (layout.cell_w, layout.cell_h),
                (px_w, px_h),
            ) && x0 <= 0.22
                && 0.22 < x1
                && y0 <= 0.47
                && 0.47 < y1
            {
                (col, row) = (layout.cell_x + c, layout.cell_y + r);
                break 'outer;
            }
        }
    }
    assert!(col > 0, "some cell must cover the link's page position");
    assert_eq!(app.editor.pdf_page(), Some(1));
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert_eq!(
        app.editor.pdf_page(),
        Some(2),
        "clicking the internal link must flip the preview to page 2; status: {}",
        app.status
    );
}

#[test]
fn cmd_w_closes_a_sheet_tab() {
    // Same swallow bug, sheet branch: handle_sheet_key returned before
    // the close-tab handler so Cmd+W never reached close_active.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let csv = tmp.path().join("data.csv");
    std::fs::write(&csv, "name,age\nAlice,30\n").unwrap();
    app.editor.open(&csv).unwrap();
    app.focus = Pane::Editor;
    assert!(app.editor.sheet.is_some(), "setup: sheet must be open");

    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();

    assert!(
        app.editor.sheet.is_none(),
        "Cmd+W on a sheet view must close the tab, not be swallowed by handle_sheet_key"
    );
}

#[test]
fn cmd_w_closes_an_image_tab() {
    // Same swallow bug, image branch: the read-only image guard returned
    // before the close-tab handler so Cmd+W could never close a PNG tab.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let png = tmp.path().join("pic.png");
    image::RgbaImage::new(2, 2).save(&png).unwrap();
    app.editor.open(&png).unwrap();
    app.focus = Pane::Editor;
    assert!(app.editor.image.is_some(), "setup: image must be open");

    app.handle_key(key(KeyCode::Char('w'), KeyModifiers::SUPER))
        .unwrap();

    assert!(
        app.editor.image.is_none(),
        "Cmd+W on an image view must close the tab, not be swallowed by the read-only image guard"
    );
}

#[test]
fn xlsx_grid_editing_saves_cells_and_holds_formulas_for_consent() {
    // #178 end-to-end: xlsx cells edit like CSV, Cmd+S applies exactly
    // the touched cells through umya, and a formula cell needs the
    // second (forced) save to be overwritten.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("book.xlsx");
    let mut book = umya_spreadsheet::new_file();
    let ws = book.sheet_mut(0).unwrap();
    ws.cell_mut((1u32, 1u32)).set_value("name");
    ws.cell_mut((2u32, 1u32)).set_value("qty");
    ws.cell_mut((1u32, 2u32)).set_value("apples");
    ws.cell_mut((2u32, 2u32)).set_value_number(3);
    ws.cell_mut((2u32, 3u32)).set_formula("SUM(B2)");
    umya_spreadsheet::writer::xlsx::write(&book, &p).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    assert!(app.editor.sheet.is_some());

    // Edit qty (0,1) to 42.
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    app.handle_sheet_key(key(KeyCode::Char('4'), KeyModifiers::NONE));
    app.handle_sheet_key(key(KeyCode::Char('2'), KeyModifiers::NONE));
    app.handle_sheet_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.editor.dirty);
    // And overwrite the formula cell (1,1).
    app.handle_sheet_key(key(KeyCode::Char('8'), KeyModifiers::NONE));
    app.handle_sheet_key(key(KeyCode::Enter, KeyModifiers::NONE));

    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        app.editor.dirty,
        "the formula cell is held back, so the tab stays dirty: {}",
        app.status
    );
    assert!(
        app.status.contains("formula"),
        "status names it: {}",
        app.status
    );
    let book2 = umya_spreadsheet::reader::xlsx::read(&p).unwrap();
    let ws2 = book2.sheet_by_name("Sheet1").unwrap();
    assert_eq!(
        ws2.cell((2u32, 2u32)).unwrap().value(),
        "42",
        "the plain cell landed on the first save"
    );
    assert!(ws2.cell((2u32, 3u32)).unwrap().is_formula());

    // Second Cmd+S consents to the formula overwrite.
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::SUPER))
        .unwrap();
    assert!(!app.editor.dirty, "everything saved: {}", app.status);
    let book3 = umya_spreadsheet::reader::xlsx::read(&p).unwrap();
    let ws3 = book3.sheet_by_name("Sheet1").unwrap();
    assert!(!ws3.cell((2u32, 3u32)).unwrap().is_formula());
    assert_eq!(ws3.cell((2u32, 3u32)).unwrap().value(), "8");
}

/// #1618: F2 is the spreadsheet "edit this cell" key, caret at the end, as
/// docs/KEYBINDINGS.md says. Rename Symbol used to swallow it, so the next
/// characters replaced the value instead of appending to it.
#[test]
fn f2_in_the_sheet_grid_edits_the_cell_with_the_caret_at_the_end() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("people.csv");
    std::fs::write(&p, "id,name\n1,Smith\n2,Lee\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    app.handle_editor_key(key(KeyCode::Down, KeyModifiers::NONE));
    app.handle_editor_key(key(KeyCode::Right, KeyModifiers::NONE));
    app.handle_editor_key(key(KeyCode::F(2), KeyModifiers::NONE));
    let editing = app.editor.sheet.as_ref().unwrap().editing.as_ref();
    assert_eq!(
        editing.map(|e| (e.value.as_str(), e.cursor)),
        Some(("Lee", 3)),
        "F2 opens the cell editor on the value, caret at the end"
    );
    for c in "son".chars() {
        app.handle_editor_key(key(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_editor_key(key(KeyCode::Enter, KeyModifiers::NONE));
    let view = app.editor.sheet.as_ref().unwrap();
    assert_eq!(
        view.sheets[0].cell(1, 1),
        "Leeson",
        "F2 appends, like Enter"
    );
    app.save();
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "id,name\n1,Smith\n2,Leeson\n"
    );
}

/// #1375 end-to-end: one cell typed into a fully quoted CSV and saved
/// changes that line only, still fully quoted.
#[test]
fn saving_a_quoted_csv_from_the_grid_changes_only_the_edited_line() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("scores.csv");
    let src = "\"id\",\"name\",\"score\"\n\"1\",\"Ada\",\"91\"\n\"2\",\"Grace\",\"88\"\n";
    std::fs::write(&p, src).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    for c in "95".chars() {
        app.handle_sheet_key(key(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_sheet_key(key(KeyCode::Enter, KeyModifiers::NONE));
    app.save();
    assert!(!app.editor.dirty);
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        src.replace("\"91\"", "\"95\"")
    );
}

#[test]
fn sheet_grid_editing_types_commits_and_saves_with_the_delimiter() {
    // #177 end-to-end: cell cursor, type-to-replace, commit-and-advance,
    // dirty mirroring, the serialisation save path, and a structure op.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("data.csv");
    std::fs::write(&p, "a,b\n1,2\n3,4\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    assert!(app.editor.sheet.is_some());

    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    app.handle_sheet_key(key(KeyCode::Char('9'), KeyModifiers::NONE));
    assert!(
        app.editor.sheet.as_ref().unwrap().editing.is_some(),
        "typing opens the in-grid editor, replacing the value"
    );
    app.handle_sheet_key(key(KeyCode::Char('!'), KeyModifiers::NONE));
    app.handle_sheet_key(key(KeyCode::Enter, KeyModifiers::NONE));
    let view = app.editor.sheet.as_ref().unwrap();
    assert_eq!(view.sheets[0].cell(0, 1), "9!", "Enter commits");
    assert_eq!(view.sheets[0].cur_row, 1, "and advances down");
    assert!(view.dirty && app.editor.dirty);

    app.save();
    assert!(!app.editor.dirty);
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "a,b\n1,9!\n3,4\n");

    // Structure op on the selected row (row index 1: "3,4").
    app.sheet_structure_op(crate::widgets::command_palette::Command::SheetDeleteRow);
    assert!(app.editor.dirty);
    app.save();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "a,b\n1,9!\n");

    // Esc cancels an edit without committing.
    app.handle_sheet_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
    app.handle_sheet_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.editor.sheet.as_ref().unwrap().editing.is_none());
    assert_eq!(
        app.editor.sheet.as_ref().unwrap().sheets[0].cell(0, 1),
        "9!"
    );
}

/// `stock.csv` open in the grid, as #1140 repros it.
fn stock_sheet_app() -> (tempfile::TempDir, std::path::PathBuf, App) {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("stock.csv");
    std::fs::write(&p, "item,qty\nbolt,4\nnut,9\nwasher,25\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    assert!(app.editor.sheet.is_some());
    (tmp, p, app)
}

/// #1140: Cmd+S with a cell still being typed into saves that cell along
/// with the ones already committed.
#[test]
fn saving_a_sheet_saves_the_cell_still_being_typed_into() {
    let (_tmp, p, mut app) = stock_sheet_app();
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_sheet(&mut app, "40");
    app.handle_sheet_key(key(KeyCode::Enter, KeyModifiers::NONE));
    type_into_sheet(&mut app, "90");
    app.save();
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "item,qty\nbolt,40\nnut,90\nwasher,25\n"
    );
    assert!(!app.editor.dirty, "{}", app.status);
    assert!(app.editor.sheet.as_ref().unwrap().editing.is_none());
}

/// #1140: with the typed cell the only change, Cmd+S saves it instead of
/// calling the editable grid a read-only preview.
#[test]
fn saving_a_sheet_whose_only_change_is_being_typed_saves_it() {
    let (_tmp, p, mut app) = stock_sheet_app();
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_sheet(&mut app, "40");
    app.save();
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "item,qty\nbolt,40\nnut,9\nwasher,25\n",
        "{}",
        app.status
    );
    assert!(app.status.starts_with("Saved"), "{}", app.status);
}

/// An unedited sheet writes nothing, and says there's nothing to save
/// rather than that the grid is read-only.
#[test]
fn saving_an_unedited_sheet_writes_nothing_and_says_so() {
    let (_tmp, p, mut app) = stock_sheet_app();
    let before = std::fs::metadata(&p).unwrap().modified().unwrap();
    app.save();
    assert_eq!(app.status, "No changes to save");
    assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), before);
}

/// Esc still throws a typed value away: the save that follows has nothing
/// to write.
#[test]
fn a_cell_edit_cancelled_with_esc_is_not_saved() {
    let (_tmp, p, mut app) = stock_sheet_app();
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_sheet(&mut app, "40");
    app.handle_sheet_key(key(KeyCode::Esc, KeyModifiers::NONE));
    app.save();
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "item,qty\nbolt,4\nnut,9\nwasher,25\n"
    );
}

fn xlsx_value(p: &std::path::Path, col: u32, row: u32) -> String {
    let book = umya_spreadsheet::reader::xlsx::read(p).unwrap();
    book.sheet(0)
        .unwrap()
        .cell((col, row))
        .unwrap()
        .value()
        .to_string()
}

/// #1141: an xlsx cell committed by clicking another cell is saved.
#[test]
fn an_xlsx_cell_committed_by_a_click_is_saved() {
    let (_tmp, p, mut app) = prices_xlsx_app();
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_sheet(&mut app, "5");
    let (x, y) = sheet_cell_on_screen(&app, 2, 0);
    left_click(&mut app, x, y);
    assert!(app.editor.dirty, "the click commits the edit");
    app.save();
    assert_eq!(xlsx_value(&p, 2, 2), "5", "{}", app.status);
    assert!(app.status.ends_with("(1 cell)"), "{}", app.status);
    assert!(!app.editor.dirty);
}

/// #1140 (hex comment): half a byte typed is not a read-only preview.
#[test]
fn a_hex_tab_with_half_a_byte_typed_is_not_called_read_only() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("blob.bin");
    std::fs::write(&p, vec![0u8; 16]).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    app.handle_hex_key(key(KeyCode::Char('f'), KeyModifiers::NONE));
    app.save();
    assert!(!app.status.contains("read-only"), "{}", app.status);
    assert_eq!(std::fs::read(&p).unwrap(), vec![0u8; 16]);
}

/// stock.csv from #1137: the widget row's unquoted `1,200` gives it the
/// only third field, so deleting it shrinks the sheet to two columns.
fn sheet_app_on_stock_csv() -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("stock.csv");
    std::fs::write(&p, "item,qty\nbolt,4\nwidget,1,200\nnut,9\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    (tmp, app)
}

#[test]
fn deleting_the_only_row_reaching_the_last_column_keeps_the_caret_in_the_sheet() {
    let (_tmp, mut app) = sheet_app_on_stock_csv();
    app.handle_sheet_key(key(KeyCode::Down, KeyModifiers::NONE));
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    let data = &app.editor.sheet.as_ref().unwrap().sheets[0];
    assert_eq!((data.cur_row, data.cur_col, data.col_count()), (1, 2, 3));

    // Panicked with "range end index 2 out of range for slice of length 2".
    app.sheet_structure_op(crate::widgets::command_palette::Command::SheetDeleteRow);
    let data = &app.editor.sheet.as_ref().unwrap().sheets[0];
    assert_eq!(data.col_count(), 2);
    assert_eq!(data.cur_col, 1, "the caret moves onto the last column left");
    assert!(data.scroll_col <= data.cur_col);
    assert_eq!(data.cell(1, 0), "nut");
    // The grid still moves and renders from there.
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    app.handle_sheet_key(key(KeyCode::Left, KeyModifiers::NONE));
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
}

#[test]
fn deleting_a_row_that_leaves_the_column_count_alone_keeps_the_caret_column() {
    // Negative: bolt is not the widest row, so the sheet keeps its three
    // columns and the caret stays where it was.
    let (_tmp, mut app) = sheet_app_on_stock_csv();
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    app.sheet_structure_op(crate::widgets::command_palette::Command::SheetDeleteRow);
    let data = &app.editor.sheet.as_ref().unwrap().sheets[0];
    assert_eq!(data.col_count(), 3);
    assert_eq!((data.cur_row, data.cur_col), (0, 1));
    assert_eq!(data.cell(0, 0), "widget");
}

#[test]
fn hex_typing_edits_bytes_and_cmd_s_writes_them_in_place() {
    // #173 end-to-end: hex-pane nibble typing, Tab to the ASCII pane,
    // dirty tracking, undo, and the byte save path (never the text
    // choke point, which #185 makes refuse preview tabs).
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("blob.bin");
    std::fs::write(&p, vec![0u8; 16]).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    assert!(app.editor.hex.is_some());

    // "4" then "1" completes 0x41 at offset 0 and advances the cursor.
    app.handle_hex_key(key(KeyCode::Char('4'), KeyModifiers::NONE));
    assert_eq!(
        app.editor.hex.as_ref().unwrap().pending_nibble,
        Some(4),
        "first nibble latches"
    );
    app.handle_hex_key(key(KeyCode::Char('1'), KeyModifiers::NONE));
    let v = app.editor.hex.as_ref().unwrap();
    assert_eq!(v.effective_byte(0), Some(0x41));
    assert_eq!(v.cursor, 1, "completed byte advances");
    assert!(app.editor.dirty, "pending edits mark the tab dirty");

    // Tab switches panes; a typed character lands as its byte.
    app.handle_hex_key(key(KeyCode::Tab, KeyModifiers::NONE));
    app.handle_hex_key(key(KeyCode::Char('Z'), KeyModifiers::NONE));
    let v = app.editor.hex.as_ref().unwrap();
    assert_eq!(v.effective_byte(1), Some(b'Z'));
    assert_eq!(v.cursor, 2);

    // Undo removes the last byte; redo restores it.
    app.handle_hex_key(key(KeyCode::Char('z'), KeyModifiers::SUPER));
    assert_eq!(
        app.editor.hex.as_ref().unwrap().effective_byte(1),
        Some(0),
        "undo reverts to the disk byte"
    );
    app.handle_hex_key(key(
        KeyCode::Char('z'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ));
    assert_eq!(
        app.editor.hex.as_ref().unwrap().effective_byte(1),
        Some(b'Z')
    );

    // Cmd+S through the REAL key dispatch writes the bytes in place
    // and cleans the tab (#191 review: app.save() directly would keep
    // passing if the chord routing broke).
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::SUPER))
        .unwrap();
    assert!(!app.editor.dirty);
    let disk = std::fs::read(&p).unwrap();
    assert_eq!(disk[0], 0x41);
    assert_eq!(disk[1], b'Z');
    assert_eq!(disk.len(), 16, "in-place write, length untouched");
    assert!(
        app.status.starts_with("Wrote 2 bytes"),
        "status reports the write: {}",
        app.status
    );
}

/// The firmware image from #1644: the text "1234" at 0x115 and 0x137, and
/// the bytes 12 34 at 0x11A.
fn app_with_firmware_in_hex(tmp: &std::path::Path) -> App {
    let f = tmp.join("fw.bin");
    // 256 bytes of non-ASCII lead-in, so no digits come before the text.
    let mut bytes: Vec<u8> = (0x80..=0xff).chain(0x80..=0xff).collect();
    bytes.extend_from_slice(b"version 2024.1 build 1234\x00\x12\x34");
    bytes.extend_from_slice(&[0; 20]);
    bytes.extend_from_slice(b"serial 1234\x00");
    std::fs::write(&f, bytes).unwrap();
    let mut app = App::new(tmp.to_path_buf()).unwrap();
    app.editor.open_hex(&f).unwrap();
    app
}

fn hex_find(app: &mut App, query: &str) {
    app.open_hex_find_prompt();
    app.input_prompt.as_mut().unwrap().value = query.to_string();
    app.submit_input_prompt();
}

/// #1644: `"1234"` in quotes finds the text, then the next one on F3, and
/// the status line says it searched for text.
#[test]
fn hex_find_of_quoted_digits_finds_the_text() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_firmware_in_hex(tmp.path());
    hex_find(&mut app, "\"1234\"");
    assert_eq!(app.editor.hex.as_ref().unwrap().cursor, 0x115);
    assert_eq!(app.status, "Found text \"1234\" at 0x115");
    app.hex_find_next();
    assert_eq!(app.editor.hex.as_ref().unwrap().cursor, 0x137);
}

/// #1644: from the text column (Tab), a hex-shaped query is text, and the
/// prompt's title says so.
#[test]
fn hex_find_from_the_text_column_reads_digits_as_text() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_firmware_in_hex(tmp.path());
    app.editor.hex.as_mut().unwrap().ascii_focus = true;
    app.open_hex_find_prompt();
    assert_eq!(
        app.input_prompt.as_ref().unwrap().title,
        "Find in Hex (text)"
    );
    app.input_prompt.as_mut().unwrap().value = String::from("2024");
    app.submit_input_prompt();
    assert_eq!(app.editor.hex.as_ref().unwrap().cursor, 0x108);
    assert_eq!(app.status, "Found text \"2024\" at 0x108");
}

/// Negative (#1644): in the hex grid a hex-shaped query is still bytes,
/// now named in the status line, and a query that is not hex-shaped is
/// found the same from either column.
#[test]
fn hex_find_in_the_grid_still_reads_hex_pairs_as_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_firmware_in_hex(tmp.path());
    hex_find(&mut app, "12 34");
    assert_eq!(app.editor.hex.as_ref().unwrap().cursor, 0x11A);
    assert_eq!(app.status, "Found bytes 12 34 at 0x11A");
    for text_side in [false, true] {
        let mut app = app_with_firmware_in_hex(tmp.path());
        app.editor.hex.as_mut().unwrap().ascii_focus = text_side;
        hex_find(&mut app, "serial");
        assert_eq!(
            app.editor.hex.as_ref().unwrap().cursor,
            0x130,
            "{text_side}"
        );
    }
    hex_find(&mut app, "\"no such text\"");
    assert_eq!(app.status, "Not found: text \"no such text\"");
}

/// #1438: a spreadsheet cell still being typed into is an unsaved edit:
/// Commit asks first, and Save All & Commit writes the typed value.
#[test]
fn a_sheet_cell_being_typed_into_holds_the_commit() {
    let (tmp, mut app) = scm_ready_to_commit();
    let csv = tmp.path().join("stock.csv");
    std::fs::write(&csv, "item,qty\nbolt,4\n").unwrap();
    app.editor.open(&csv).unwrap();
    assert!(app.editor.sheet.is_some());
    app.handle_sheet_key(key(KeyCode::Right, KeyModifiers::NONE));
    type_into_sheet(&mut app, "40");
    assert!(!app.editor.dirty, "the typed cell is not committed yet");
    app.source_control.message = "bump seed".to_string();
    app.source_control.message_cursor = app.source_control.message.chars().count();
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    let pending = app.pending_unsaved_scm.as_ref().expect("the prompt opens");
    assert_eq!(pending.files, vec![csv.clone()]);
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    wait_for_git_net(&mut app);
    assert!(app.pending_unsaved_scm.is_none());
    assert_eq!(
        std::fs::read_to_string(&csv).unwrap(),
        "item,qty\nbolt,40\n",
        "{}",
        app.status
    );
}

/// A swap into a viewer drops the map (#349 review): the hex view's single
/// placeholder line was written by no seat, and a save from the viewer must
/// not persist the text tab's map against bytes it never described. The swap
/// back to text is an ordinary reopen and gets the persisted map again.
#[test]
fn a_viewer_swap_drops_the_map_and_the_text_reopen_gets_it_back() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    std::fs::write(&f, "one\n").unwrap();
    let mut seats = crate::provenance::Provenance::new();
    seats.record(0..1, Seat::Navigator);
    crate::history::record_with_seats_in(
        hist.path(),
        &f,
        &std::fs::read(&f).unwrap(),
        1_000,
        &seats,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    app.sync_provenance();
    assert_eq!(
        app.editor.provenance, seats,
        "staging: the text tab is synced"
    );
    app.editor.open_hex(&f).unwrap();
    assert!(app.editor.hex.is_some(), "staging: the tab is a hex view");
    assert_eq!(
        app.editor.provenance.attributed(),
        0,
        "the swap into the viewer dropped the map"
    );
    app.sync_provenance();
    assert_eq!(
        app.editor.provenance.attributed(),
        0,
        "and a sync does not credit the viewer's placeholder line"
    );
    assert_eq!(
        app.editor.provenance_to_record().attributed(),
        0,
        "a save from the viewer would record no map"
    );
    app.editor.open(&f).unwrap();
    app.sync_provenance();
    assert_eq!(
        app.editor.provenance, seats,
        "the reopen as text gets the persisted map back"
    );
}

/// A markdown preview is not a viewer (#349 review): `lines` still holds the
/// file's own text and the tab is saveable, so a save taken while the
/// preview is up records the map rather than dropping it (and, inside the
/// merge window, deleting the previous snapshot's sidecar with nothing to
/// replace it).
#[test]
fn a_markdown_preview_save_keeps_the_map() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.md");
    std::fs::write(&f, "# one\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.provenance.record(0..1, Seat::Navigator);
    assert!(
        app.editor.toggle_markdown_preview(),
        "staging: the preview is up"
    );
    assert!(
        !app.editor.has_non_text_view(),
        "staging: a markdown preview tab is still saveable text"
    );
    assert_eq!(
        app.editor.provenance_to_record().seat(0),
        Some(&Seat::Navigator),
        "the buffer's own text is in `lines`; its map still describes it"
    );
    app.editor.dirty = true;
    app.save();
    let snaps = wait_for_snapshots(&app.history_root, &f);
    assert_eq!(snaps.len(), 1, "staging: the save recorded a snapshot");
    wait_for_seats(&app.history_root, &f, snaps[0].millis);
}

/// #465: a viewer runs the declared program on the file in a pane of its own,
/// with `{file}` replaced by the file's path, and the pane takes focus. The
/// program here is `sh`, so nothing has to be installed for the test.
#[test]
fn opening_a_file_in_a_viewer_runs_the_tool_on_it_in_a_new_terminal_pane() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data.csv");
    std::fs::write(&data, "a,b\n1,2\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.consented_extensions.insert("csvlens".into());
    let before = app.terminals.len();
    let viewer = crate::mcp::registry::ContributedViewer {
        ext_id: "csvlens".into(),
        id: "csvlens".into(),
        label: "Open in csvlens".into(),
        command: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            "echo VIEWING \"$1\"; sleep 30".into(),
            "sh".into(),
            "{file}".into(),
        ],
        extensions: vec!["csv".into()],
        provision: None,
    };
    app.open_in_viewer(&viewer, &data);
    assert_eq!(
        app.terminals.len(),
        before + 1,
        "the viewer gets a pane of its own"
    );
    assert!(app.show_terminal, "the panel is shown");
    assert!(
        matches!(app.focus, Pane::Terminal),
        "and the pane takes focus"
    );
    let want = format!("VIEWING {}", data.display());
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the viewer to print the file it was handed",
        || app.terminal().visible_text().contains(&want),
    );
}

/// A file name that is not valid UTF-8 must not be handed to the viewer
/// mangled (#485 review): the tool would open a different path and fail.
/// Until the pane spawn can carry OS-native arguments, such a file is
/// refused with a status line rather than opened wrong.
#[cfg(unix)]
#[test]
fn a_viewer_refuses_a_path_it_cannot_pass_faithfully() {
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir().unwrap();
    // The file is deliberately not created: APFS refuses such a name, and
    // the guard fires before the viewer path touches the disk anyway.
    let odd = tmp
        .path()
        .join(std::ffi::OsStr::from_bytes(b"d\xffata.csv"));
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let viewer = crate::mcp::registry::ContributedViewer {
        ext_id: "csvlens".into(),
        id: "csvlens".into(),
        label: "Open in csvlens".into(),
        command: "/bin/sh".into(),
        args: vec!["-c".into(), "sleep 30".into(), "sh".into(), "{file}".into()],
        extensions: vec!["csv".into()],
        provision: None,
    };
    let before = app.terminals.len();
    app.open_in_viewer(&viewer, &odd);
    assert_eq!(
        app.terminals.len(),
        before,
        "no pane is spawned for a path that cannot be passed"
    );
    assert!(
        app.status.contains("not valid UTF-8"),
        "the status says why: {:?}",
        app.status
    );
}

/// A viewer spawns a program from a manifest, so it passes the same
/// first-run consent gate a sidecar does (#485 review): the first use of an
/// extension's viewer shows the exact command line and spawns nothing until
/// the user allows it; a consented extension runs straight away.
#[test]
fn a_viewer_asks_for_consent_before_its_first_run() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data.csv");
    std::fs::write(&data, "a,b\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let viewer = crate::mcp::registry::ContributedViewer {
        ext_id: format!("consent-test-{}", std::process::id()),
        id: "csvlens".into(),
        label: "Open in csvlens".into(),
        command: "/bin/sh".into(),
        args: vec!["-c".into(), "sleep 30".into(), "sh".into(), "{file}".into()],
        extensions: vec!["csv".into()],
        provision: None,
    };
    let before = app.terminals.len();
    app.open_in_viewer(&viewer, &data);
    assert_eq!(app.terminals.len(), before, "nothing spawns before consent");
    let prompt = app
        .input_prompt
        .as_ref()
        .expect("the consent prompt is open");
    assert!(
        prompt.title.contains(&viewer.ext_id) && prompt.title.contains("/bin/sh"),
        "the prompt names the extension and the exact command: {:?}",
        prompt.title
    );
}

/// Allowing a viewer records the consent and resumes the open that asked;
/// a prefs write that fails is said in the status rather than swallowed.
#[test]
fn allowing_a_viewer_records_consent_and_resumes_the_open() {
    const EXT: &str = r#"
id = "tviewer"
name = "tviewer"
api_version = 1
[[viewers]]
id = "sh"
label = "Open in sh"
command = "/bin/sh"
args = ["-c", "sleep 30", "sh", "{file}"]
extensions = ["csv"]
"#;
    let (_scratch, croft) = scratch_config(&[("tviewer", EXT)]);
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data.csv");
    std::fs::write(&data, "a,b\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.config_dir = croft.clone();
    app.consented_extensions = consent_on_disk(&croft);
    let viewer = crate::mcp::registry::viewer_by_id_in_dir(&croft, "tviewer/sh")
        .expect("the scratch viewer");
    let before = app.terminals.len();
    app.open_in_viewer(&viewer, &data);
    assert!(
        app.input_prompt.is_some(),
        "fixture: consent is asked first"
    );
    app.submit_input_prompt();
    assert_eq!(app.terminals.len(), before + 1, "allowing resumes the open");
    assert!(
        app.consented_extensions.contains("tviewer"),
        "and records the consent"
    );
    let saved = consent_on_disk(&croft);
    assert!(saved.contains("tviewer"), "on disk too: {saved:?}");
    assert!(
        !app.status.contains("could not be saved"),
        "a successful save is not reported as a failure: {:?}",
        app.status
    );
    // Now with the prefs file made unwritable (a directory in its place):
    // the consent still holds for the session and the status says the save
    // failed, instead of the failure vanishing.
    app.consented_extensions.remove("tviewer");
    let _ = std::fs::remove_file(croft.join("config.json"));
    std::fs::create_dir_all(croft.join("config.json")).unwrap();
    app.open_in_viewer(&viewer, &data);
    app.submit_input_prompt();
    assert!(
        app.status.contains("could not be saved"),
        "a failed prefs write is reported: {:?}",
        app.status
    );
    assert!(
        app.consented_extensions.contains("tviewer"),
        "the session grant still holds"
    );
}

/// The Explorer row and the palette row for a viewer are built from the
/// same config dir the click resolves them through: a viewer installed only
/// under the app's dir gets its row, and the row opens.
#[test]
fn a_viewer_row_is_built_and_resolved_through_the_same_config_dir() {
    const EXT: &str = r#"
id = "tviewer"
name = "tviewer"
api_version = 1
[[viewers]]
id = "sh"
label = "Open in sh"
command = "/bin/sh"
args = ["-c", "sleep 30", "sh", "{file}"]
extensions = ["csv"]
"#;
    let (_scratch, croft) = scratch_config(&[("tviewer", EXT)]);
    let tmp = tempfile::tempdir().unwrap();
    let csv = tmp.path().join("data.csv");
    std::fs::write(&csv, "a,b\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.config_dir = croft.clone();
    app.consented_extensions = consent_on_disk(&croft);
    app.consented_extensions.insert("tviewer".into());
    let n = file_node(&csv);
    let items = build_tree_context_menu_items_in_dir(
        &croft,
        Some(&n),
        tmp.path(),
        std::slice::from_ref(&csv),
        tmp.path(),
        None,
        None,
    );
    let row = items
        .iter()
        .find_map(|(label, a)| match a {
            MenuAction::OpenInViewer(id, p) if p == &csv => Some((label.clone(), id.clone())),
            _ => None,
        })
        .expect("the viewer under the app's config dir gets a row");
    assert_eq!(row, ("Open in sh".to_string(), "tviewer/sh".to_string()));
    let before = app.terminals.len();
    app.dispatch_menu_action(
        MenuAction::OpenInViewer(row.1.clone(), csv.clone()),
        tmp.path().to_path_buf(),
    );
    assert_eq!(
        app.terminals.len(),
        before + 1,
        "and the row resolves and opens"
    );
    let palette_ids: Vec<String> = crate::mcp::registry::contributed_viewer_commands_in_dir(&croft)
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert!(
        palette_ids.iter().any(|id| id == "viewer:tviewer/sh"),
        "the palette row comes from the same dir: {palette_ids:?}"
    );
}

/// A viewer that is gone by the time its menu row is clicked, or by the time
/// its consent prompt is allowed (the extension was removed in between),
/// must say so rather than do nothing: a silent click reads as a broken
/// menu. Neither path spawns anything or records consent for a viewer it
/// could not find.
#[test]
fn a_vanished_viewer_is_reported_from_the_menu_and_the_consent_prompt() {
    use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data.csv");
    std::fs::write(&data, "a,b\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let before = app.terminals.len();
    let key = format!("vanished-ext-{}/csvlens", std::process::id());
    app.status.clear();
    app.dispatch_menu_action(
        MenuAction::OpenInViewer(key.clone(), data.clone()),
        tmp.path().to_path_buf(),
    );
    assert_eq!(app.terminals.len(), before, "the menu click spawns nothing");
    assert!(
        app.status.contains("no longer"),
        "the click says the viewer is gone: {:?}",
        app.status
    );
    app.status.clear();
    app.open_input_prompt(
        InputPrompt::new(
            InputPurpose::ViewerConsent {
                key: key.clone(),
                path: data,
            },
            "Allow",
            "",
        )
        .with_value("allow"),
    );
    app.submit_input_prompt();
    assert!(app.input_prompt.is_none(), "the prompt closes");
    assert_eq!(app.terminals.len(), before, "allowing spawns nothing");
    assert!(
        app.status.contains("no longer"),
        "allowing says the viewer is gone: {:?}",
        app.status
    );
    assert!(
        !app.consented_extensions
            .iter()
            .any(|e| key.starts_with(e.as_str())),
        "no consent is recorded for a viewer that could not be found"
    );
}

/// #465: the palette row reaches the viewer route (not the MCP command path)
/// and guards on the active file before anything is spawned.
#[test]
fn a_viewer_palette_row_guards_on_the_active_file() {
    let tmp = tempfile::tempdir().unwrap();
    let notes = tmp.path().join("notes.md");
    let data = tmp.path().join("data.csv");
    std::fs::write(&notes, "# hi\n").unwrap();
    std::fs::write(&data, "a,b\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.consented_extensions.insert("csvlens".into());
    // The `viewer:` prefix is routed before the MCP lookup: an unknown viewer
    // is reported as a viewer, not as an extension command.
    app.run_extension_command("viewer:nope");
    assert_eq!(app.status, "Viewer 'nope' is unavailable");

    let viewer = crate::mcp::registry::ContributedViewer {
        ext_id: "csvlens".into(),
        id: "csvlens".into(),
        label: "Open in csvlens".into(),
        command: "/bin/sh".into(),
        args: vec!["-c".into(), "sleep 30".into()],
        extensions: vec!["csv".into(), "tsv".into()],
        provision: None,
    };
    let before = app.terminals.len();
    app.editor.path = None;
    app.open_active_file_in_viewer(&viewer);
    assert_eq!(app.status, "Open in csvlens: open a file first");
    app.editor.open(&notes).unwrap();
    app.open_active_file_in_viewer(&viewer);
    assert_eq!(
        app.status,
        "Open in csvlens: the active file is not one of .csv / .tsv"
    );
    assert_eq!(app.terminals.len(), before, "a guard spawns nothing");
    app.editor.open(&data).unwrap();
    app.open_active_file_in_viewer(&viewer);
    assert_eq!(
        app.terminals.len(),
        before + 1,
        "a matching file opens in a pane"
    );
}

#[test]
fn iterm2_image_drag_artefact_does_not_paste_its_path_into_the_editor() {
    // Root cause of the user's "Pasted 67 chars" surprise: when iTerm2
    // hijacks a click-drag on an OSC-1337 activity-bar icon, it writes
    // the decoded PNG to $TMPDIR/iTerm2.<rand>.png and re-injects the
    // path as a bracketed paste. parse_dropped_paths filters those
    // paths out of the IMPORT list, but without an early-return in
    // handle_paste the fallthrough would still insert the raw path
    // text into the focused editor.
    let mut app = editor_app_with_lines(&[""]);
    app.focus_pane(Pane::Editor);
    let tmp = tempfile::tempdir().unwrap();
    let temp_dir = tmp.path().join("T").join("com.googlecode.iterm2");
    std::fs::create_dir_all(&temp_dir).unwrap();
    let fake = temp_dir.join("iTerm2.5AO7K5.png");
    std::fs::write(&fake, b"\x89PNG\r\n\x1a\n").unwrap();
    let payload = format!("{}\n", fake.display());
    app.handle_paste(&payload);
    assert_eq!(
        app.editor.lines,
        vec![""],
        "iTerm2 inline-image drag artefact must be silently dropped at every level — the path string must NOT be pasted as text into the editor body (the user reported a CLAUDE.local.md tab opening with 'Pasted 67 chars' after trying to resize the sidebar near the Explorer icon)"
    );
}

#[test]
fn parse_dropped_paths_drops_iterm2_inline_image_drag_artefact() {
    // iTerm2 lets the user drag any OSC-1337 inline image (which is
    // how croft paints activity-bar icons / welcome wordmark / SSH
    // hero) out of the terminal. On mouse-up iTerm2 writes the bytes
    // to $TMPDIR/iTerm2.<rand>.png and re-injects the path as a
    // bracketed paste, which croft would otherwise see as "the user
    // dragged a file into the editor" and open as an image preview.
    // That has happened to the user accidentally when they tried to
    // click a sidebar icon and the click drifted into a drag — the
    // file `iTerm2.5AO7K5.png` opened as a tab showing the Explorer
    // glyph. The drop must be silently filtered.
    let tmp = tempfile::tempdir().unwrap();
    let temp_dir = tmp.path().join("T").join("com.googlecode.iterm2");
    std::fs::create_dir_all(&temp_dir).unwrap();
    let fake = temp_dir.join("iTerm2.5AO7K5.png");
    std::fs::write(&fake, b"\x89PNG\r\n\x1a\n").unwrap();
    let parsed = parse_dropped_paths(&format!("{}\n", fake.display()));
    assert!(
        parsed.is_empty(),
        "iTerm2's inline-image drag temp file must be filtered out so a stray click-drag on an activity-bar icon does not open a phantom PNG tab — got {parsed:?}"
    );
}

#[test]
fn closing_image_tab_requests_overlay_clear_on_next_render() {
    // Repro for: opening a PNG/PDF then closing the tab leaves the
    // OSC-1337 pixels bleeding through the welcome screen. The render
    // path that swaps to welcome must mark the editor image overlay
    // for clearing so the main loop wipes the screen.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.editor[0].set_test_state(
        Some(String::from("dummy-osc")),
        Some(EditorImageLayout {
            cell_x: 0,
            cell_y: 0,
            cell_w: 10,
            cell_h: 10,
            path: tmp.path().join("doomed.png"),
            page: 1,
            generation: 0,
        }),
        true,
    );
    // Editor is blank-initial right after construction, so calling
    // disable_editor_image directly mimics what render() does on
    // the welcome branch.
    app.disable_editor_image(0);
    assert!(app.overlays.editor[0].image_is_none());
    assert!(app.overlays.editor[0].layout_is_none());
    assert!(
        app.consume_editor_image_clear(),
        "first call must report a pending clear"
    );
    assert!(
        !app.consume_editor_image_clear(),
        "second call must be a no-op"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn iterm2_and_terminal_scripts_shell_quote_paths_with_spaces() {
    let exe = std::path::Path::new("/usr/local/bin/croft");
    let root = std::path::Path::new("/home/me/My Project");
    let file = std::path::Path::new("/home/me/My Project/a b.rs");

    let iterm = iterm2_new_window_script(exe, root, file);
    assert!(
        iterm
            .contains(r#"tell application "iTerm" to create window with default profile command "#),
        "{iterm}"
    );
    // Each path single-quoted so the space survives the shell.
    assert!(iterm.contains("'/home/me/My Project'"), "{iterm}");
    assert!(iterm.contains("'/home/me/My Project/a b.rs'"), "{iterm}");

    // The window opens focused on the file: --zen is part of the invocation.
    assert!(iterm.contains("--zen"), "{iterm}");

    let term = apple_terminal_new_window_script(exe, root, file);
    assert!(
        term.contains(r#"tell application "Terminal" to do script "#),
        "{term}"
    );
    assert!(
        term.contains("--open-file '/home/me/My Project/a b.rs' --zen"),
        "{term}"
    );
}

/// There are physically two image-overlay slots (left/right column); any
/// layout other than the two-column split bakes the active group into slot 0.
/// The focused side must therefore never index past the array: clicking a PDF
/// in the lower-right group of a three-way split panicked with
/// index-out-of-bounds before this was clamped.
#[test]
fn focused_image_side_stays_within_the_two_overlay_slots() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "hello");
    app.split_editor_dir(editor_layout::SplitDir::Horizontal, true);
    app.split_editor_dir(editor_layout::SplitDir::Vertical, true);
    assert_eq!(app.editor_layout.active_dfs_index(), 2);
    assert!(
        app.focused_image_side() < 2,
        "focused_image_side must stay within overlays.editor"
    );
}

#[test]
fn editor_image_overlay_is_dual_slot_and_clear_ors_both_columns() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let layout = |name: &str| EditorImageLayout {
        cell_x: 0,
        cell_y: 0,
        cell_w: 10,
        cell_h: 10,
        path: tmp.path().join(name),
        page: 1,
        generation: 0,
    };
    // Both physical columns can hold an inline image at once.
    app.overlays.editor[0].set_test_state(Some("left-osc".into()), Some(layout("l.png")), true);
    app.overlays.editor[1].set_test_state(Some("right-osc".into()), Some(layout("r.png")), true);
    assert_eq!(
        app.editor_image_payload(0).map(|(o, _)| o),
        Some("left-osc")
    );
    assert_eq!(
        app.editor_image_payload(1).map(|(o, _)| o),
        Some("right-osc")
    );

    // Disabling the right slot arms a clear; `consume_editor_image_clear`
    // must report it even though the left slot is quiet.
    app.disable_editor_image(1);
    assert!(
        app.consume_editor_image_clear(),
        "a clear armed on either column must surface from the ORed consume"
    );
    assert!(
        !app.consume_editor_image_clear(),
        "the latch is one-shot and both slots were consumed"
    );
}

/// Issue #215: text in the rendered Markdown view must be selectable.
/// A drag over the preview selects what is on screen, Cmd+C copies the
/// RENDERED text (not the source), and Esc clears the selection.
#[test]
fn dragging_in_the_markdown_preview_selects_and_copies_rendered_text() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let md = tmp.path().join("doc.md");
    std::fs::write(&md, "# Heading\n\nSome rendered body text here.\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&md).unwrap();
    app.focus_pane(Pane::Editor);
    // Cmd+Shift+V flips to the rendered preview.
    app.handle_key(key(
        KeyCode::Char('v'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(
        app.editor.markdown_preview.is_some(),
        "the preview must be up"
    );
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();

    // Find the rendered row carrying the body text and drag across it.
    let (area, row_idx, text) = {
        let md = app.editor.markdown_preview.as_ref().unwrap();
        let (i, t) = md
            .rows
            .iter()
            .enumerate()
            .map(|(i, cells)| (i as u16, cells.concat()))
            .find(|(_, r)| r.contains("rendered body"))
            .expect("the body text must be rendered");
        (md.last_area, i, t)
    };
    let start_col = text.find("rendered").unwrap() as u16;
    let end_col = start_col + "rendered".len() as u16 - 1;
    let y = area.y + row_idx;
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        area.x + start_col,
        y,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        area.x + end_col,
        y,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        area.x + end_col,
        y,
    ));
    assert_eq!(
        app.editor
            .markdown_preview
            .as_ref()
            .map(|md| md.selection_text())
            .unwrap_or_default(),
        "rendered",
        "the drag must select the rendered word under the pointer"
    );
    // The selection paints: re-render and check the tinted cell.
    term.draw(|frame| app.render(frame)).unwrap();
    let sel_bg = app.theme.selection();
    assert_eq!(
        term.backend().buffer()[(area.x + start_col, y)].bg,
        sel_bg,
        "the selected cell must wear the selection background"
    );
    // Esc clears it.
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(
        !app.editor
            .markdown_preview
            .as_ref()
            .map(|md| md.has_selection())
            .unwrap_or(false),
        "Esc clears the preview selection"
    );
}

#[test]
fn image_eviction_wipes_the_screen_only_on_cell_buffer_protocols() {
    // The `terminal.clear()` the main loop fires when an image-clear latch is
    // armed exists to evict CACHED IMAGE CELLS: iTerm2 OSC-1337 and sixel
    // graphics live in the normal cell buffer, so only a `2J` removes them.
    // Kitty graphics live on their own layer and are deleted by
    // `evict_inline_images_on_clear` (delete-all-by-escape) without touching a
    // single text cell - wiping the screen there blanks the whole app for a
    // full SSH round trip on every latch (the "croft blinks while I type" bug).
    use crate::iterm2_inline::InlineImageProtocol;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.inline_protocol = InlineImageProtocol::Kitty;
    assert!(
        !app.image_eviction_needs_screen_wipe(),
        "Kitty images are deleted on their own layer; the text buffer must stay intact"
    );
    app.inline_protocol = InlineImageProtocol::ITerm2;
    assert!(app.image_eviction_needs_screen_wipe());
    app.inline_protocol = InlineImageProtocol::Sixel;
    assert!(app.image_eviction_needs_screen_wipe());
    app.inline_protocol = InlineImageProtocol::None;
    assert!(app.image_eviction_needs_screen_wipe());
}

/// #1591: with no inline-image protocol (tmux, plain SSH) an image tab
/// draws its picture in half blocks; a terminal that shows inline images
/// gets the empty canvas it paints over, as before.
#[test]
fn an_image_tab_draws_half_blocks_without_an_inline_image_protocol() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("shot.png");
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(6, 6, image::Rgba([10, 200, 30, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode test png");
    std::fs::write(&path, &png).unwrap();
    let half_blocks = |protocol| {
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.inline_protocol = protocol;
        app.editor.open(&path).unwrap();
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let buf = term.backend().buffer().clone();
        buf.content()
            .iter()
            .filter(|c| c.symbol() == "\u{2580}" && c.fg == Color::Rgb(10, 200, 30))
            .count()
    };
    use crate::iterm2_inline::InlineImageProtocol;
    assert_eq!(
        half_blocks(InlineImageProtocol::None),
        18,
        "6×6 pixels, 6×3 cells"
    );
    assert_eq!(half_blocks(InlineImageProtocol::Kitty), 0);
    assert_eq!(half_blocks(InlineImageProtocol::ITerm2), 0);
}

/// A right-click menu opened over an image tab was painted *under* the
/// picture: on Kitty the editor preview was placed at the default z=0, which
/// the spec puts above text. Placing it below text AND below non-default
/// background cells (the same trick the sidebar illustrations use) lets any
/// opaque popup box paint fully on top, while the preview still shows through
/// the pane's default-bg canvas.
#[test]
fn editor_image_preview_sits_below_text_so_popups_paint_over_it() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("shot.png");
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(4, 4, image::Rgba([10, 200, 30, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode test png");
    std::fs::write(&path, &png).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::Kitty;
    app.cell_pixel = Some((8, 16));
    app.editor.open(&path).unwrap();
    app.update_editor_image_overlay(
        0,
        ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        },
    );

    let (osc, _) = app.editor_image_payload(0).expect("preview must bake");
    assert!(
        osc.contains(&format!(
            "z={}",
            crate::iterm2_inline::KITTY_Z_BELOW_TEXT_AND_BG
        )),
        "the preview must be placed below text so menus draw on top: {:?}",
        &osc[..osc.len().min(120)]
    );
}

fn current_pdf_page(app: &App) -> u32 {
    app.editor
        .image
        .as_ref()
        .and_then(|i| i.pdf.as_ref())
        .expect("a PDF tab must carry page state")
        .current_page
}

fn sheet_cursor(app: &App) -> (usize, usize) {
    let sheet = app.editor.sheet.as_ref().expect("a spreadsheet tab");
    let data = &sheet.sheets[sheet.current_sheet];
    (data.cur_row, data.cur_col)
}

fn sheet_scroll(app: &App) -> (usize, usize) {
    let sheet = app.editor.sheet.as_ref().expect("a spreadsheet tab");
    let data = &sheet.sheets[sheet.current_sheet];
    (data.scroll_row, data.scroll_col)
}

#[test]
fn arrow_keys_page_through_an_open_pdf() {
    require_pdf_backend();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("doc.pdf");
    write_three_page_pdf(&path);

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&path).unwrap();
    app.focus_pane(Pane::Editor);
    assert_eq!(current_pdf_page(&app), 1);

    app.handle_key(key(KeyCode::Right, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(current_pdf_page(&app), 2, "Right must go to the next page");

    app.handle_key(key(KeyCode::Left, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(current_pdf_page(&app), 1, "Left must go back a page");

    app.handle_key(key(KeyCode::PageDown, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(current_pdf_page(&app), 2, "PageDown pages forward");

    app.handle_key(key(KeyCode::End, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(current_pdf_page(&app), 3, "End jumps to the last page");

    app.handle_key(key(KeyCode::Home, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(current_pdf_page(&app), 1, "Home jumps back to page 1");
}

#[test]
fn a_pdf_opened_from_the_explorer_pages_with_the_arrow_keys() {
    require_pdf_backend();
    let tmp = tempfile::tempdir().unwrap();
    write_three_page_pdf(&tmp.path().join("doc.pdf"));

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    open_from_explorer(&mut app, "doc.pdf");
    assert!(
        matches!(app.focus, Pane::Editor),
        "opening a file must hand the keyboard to the editor, or its navigation keys are dead"
    );

    app.handle_key(key(KeyCode::Right, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        current_pdf_page(&app),
        2,
        "a PDF opened from the Explorer must page with Right"
    );
}

#[test]
fn the_wheel_pages_a_pdf_under_the_pointer() {
    require_pdf_backend();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("doc.pdf");
    write_three_page_pdf(&path);

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&path).unwrap();
    let (col, row) = place_editor_pane(&mut app);

    wheel(&mut app, MouseEventKind::ScrollDown, col, row);
    assert_eq!(current_pdf_page(&app), 2, "wheel down pages forward");
    wheel(&mut app, MouseEventKind::ScrollUp, col, row);
    assert_eq!(current_pdf_page(&app), 1, "wheel up pages back");
}

/// A momentum flick delivers a burst of same-direction wheel events, and
/// every PDF page step is a synchronous rasteriser run on the UI thread: the
/// burst must coalesce (rate-limited per direction) instead of queueing
/// dozens of blocking renders. A reversal is deliberate and passes through
/// immediately (the paging test above depends on that).
#[test]
fn a_wheel_flick_over_a_pdf_coalesces_to_one_page_step() {
    require_pdf_backend();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("doc.pdf");
    write_three_page_pdf(&path);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&path).unwrap();
    let (col, row) = place_editor_pane(&mut app);
    wheel(&mut app, MouseEventKind::ScrollDown, col, row);
    wheel(&mut app, MouseEventKind::ScrollDown, col, row);
    wheel(&mut app, MouseEventKind::ScrollDown, col, row);
    assert_eq!(
        current_pdf_page(&app),
        2,
        "a same-direction burst steps one page, not one per event"
    );
}

/// The wheel never wraps the document: page 1 stays put on wheel up and the
/// last page stays put on wheel down. (The arrow keys wrap deliberately;
/// no PDF viewer wraps on scroll.)
#[test]
fn the_wheel_does_not_wrap_a_pdf_at_its_ends() {
    require_pdf_backend();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("doc.pdf");
    write_three_page_pdf(&path);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&path).unwrap();
    let (col, row) = place_editor_pane(&mut app);
    wheel(&mut app, MouseEventKind::ScrollUp, col, row);
    assert_eq!(
        current_pdf_page(&app),
        1,
        "wheel up on page 1 must not wrap"
    );
    app.jump_pdf_page(3);
    wheel(&mut app, MouseEventKind::ScrollDown, col, row);
    assert_eq!(
        current_pdf_page(&app),
        3,
        "wheel down on the last page must not wrap"
    );
}

/// The wheel cooldown is per document: scrolling one PDF and immediately
/// wheeling in a second, freshly focused PDF is that document's FIRST
/// gesture, not a repeat - an app-global cooldown swallowed it.
#[test]
fn the_wheel_cooldown_does_not_leak_across_pdf_documents() {
    require_pdf_backend();
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.pdf");
    let b = tmp.path().join("b.pdf");
    write_three_page_pdf(&a);
    write_three_page_pdf(&b);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&a).unwrap();
    let (col, row) = place_editor_pane(&mut app);
    wheel(&mut app, MouseEventKind::ScrollDown, col, row);
    assert_eq!(current_pdf_page(&app), 2);
    app.editor.open(&b).unwrap();
    let (col, row) = place_editor_pane(&mut app);
    // Pin the cooldown stamp to "just now, same direction, other document"
    // so the window is deterministically open regardless of how long the
    // rasteriser took: only the document identity may let this tick through.
    app.last_pdf_wheel = Some((std::time::Instant::now(), 1, Some(a.clone())));
    wheel(&mut app, MouseEventKind::ScrollDown, col, row);
    assert_eq!(
        current_pdf_page(&app),
        2,
        "the second document's first wheel tick must not be dropped"
    );
}

#[test]
fn a_spreadsheet_opened_from_the_explorer_scrolls_with_the_arrow_keys() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("data.csv"), csv_body(40)).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    open_from_explorer(&mut app, "data.csv");
    assert!(app.editor.sheet.is_some(), "a .csv opens as a spreadsheet");
    assert!(matches!(app.focus, Pane::Editor));

    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        sheet_cursor(&app).0,
        1,
        "Down moves the cell cursor down a row (#177)"
    );
    app.handle_key(key(KeyCode::Right, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        sheet_cursor(&app).1,
        1,
        "Right moves the cell cursor a column"
    );
}

#[test]
fn the_wheel_scrolls_a_spreadsheet_under_the_pointer() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("data.csv");
    std::fs::write(&path, csv_body(40)).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&path).unwrap();
    let (col, row) = place_editor_pane(&mut app);

    wheel(&mut app, MouseEventKind::ScrollDown, col, row);
    assert_eq!(sheet_scroll(&app).0, 3, "wheel down scrolls the sheet rows");
    wheel(&mut app, MouseEventKind::ScrollUp, col, row);
    assert_eq!(sheet_scroll(&app).0, 0, "wheel up scrolls back");
}

#[test]
fn a_tsv_preview_scrolls_the_same_way_as_a_csv() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("data.tsv");
    std::fs::write(&path, "a\tb\n1\t2\n3\t4\n5\t6\n7\t8\n").unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&path).unwrap();
    let (col, row) = place_editor_pane(&mut app);
    assert!(app.editor.sheet.is_some(), "a .tsv opens as a spreadsheet");

    wheel(&mut app, MouseEventKind::ScrollDown, col, row);
    assert_eq!(sheet_scroll(&app).0, 3, "wheel down scrolls a TSV too");
}

#[test]
fn the_wheel_scrolls_a_markdown_preview_under_the_pointer() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("notes.md");
    let body: String = (0..200).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&path, body).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&path).unwrap();
    let (col, row) = place_editor_pane(&mut app);
    app.toggle_markdown_preview();

    wheel(&mut app, MouseEventKind::ScrollDown, col, row);
    let scrolled = app
        .editor
        .markdown_preview
        .as_ref()
        .map(|p| p.scroll)
        .unwrap();
    assert!(scrolled > 0, "the wheel must scroll the rendered Markdown");
}

#[test]
fn an_image_preview_swallows_navigation_without_disturbing_the_buffer() {
    let tmp = tempfile::tempdir().unwrap();
    write_test_png(&tmp.path().join("shot.png"));

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    open_from_explorer(&mut app, "shot.png");
    assert!(app.editor.image.is_some());
    let (col, row) = place_editor_pane(&mut app);

    for code in [
        KeyCode::Right,
        KeyCode::Left,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::PageDown,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Char('x'),
    ] {
        app.handle_key(key(code, KeyModifiers::NONE)).unwrap();
    }
    wheel(&mut app, MouseEventKind::ScrollDown, col, row);
    wheel(&mut app, MouseEventKind::ScrollUp, col, row);
    assert_eq!(
        app.editor.lines,
        vec![String::new()],
        "an image tab is read-only: no key may reach the buffer"
    );
}

/// Root cause of the frozen preview: the overlay's re-emit key held the path
/// and the rect but not the page, so a page turn looked like "same layout" and
/// the picture on screen never changed. Baking twice across a page turn must
/// produce two different payloads.
#[test]
fn turning_a_pdf_page_rebakes_the_inline_image_overlay() {
    require_pdf_backend();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("doc.pdf");
    write_three_page_pdf(&path);

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::Kitty;
    app.cell_pixel = Some((8, 16));
    app.editor.open(&path).unwrap();
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    app.update_editor_image_overlay(0, area);
    let page1 = app
        .editor_image_payload(0)
        .map(|(osc, _)| osc.to_string())
        .expect("page 1 must bake");

    assert!(app.editor.change_pdf_page(1));
    app.update_editor_image_overlay(0, area);
    let page2 = app
        .editor_image_payload(0)
        .map(|(osc, _)| osc.to_string())
        .expect("page 2 must bake");

    assert_ne!(
        page1, page2,
        "the overlay must re-bake on a page turn, or the reader stares at a frozen page"
    );
}

/// The gesture the bug report actually used: one click on the PDF in the
/// Explorer, then the arrow keys. A single click opened the file but left the
/// keyboard on the tree, so every page-turn key walked the file list instead.
#[test]
fn a_pdf_opened_with_one_click_in_the_explorer_pages_with_the_arrow_keys() {
    require_pdf_backend();
    let tmp = tempfile::tempdir().unwrap();
    write_three_page_pdf(&tmp.path().join("doc.pdf"));

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    click_open_in_explorer(&mut app, "doc.pdf");
    assert_eq!(current_pdf_page(&app), 1, "the click must open the PDF");
    assert!(
        matches!(app.focus, Pane::Editor),
        "one click on a file must hand the keyboard to the editor, as VS Code does"
    );

    app.handle_key(key(KeyCode::Right, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(current_pdf_page(&app), 2, "Right must turn the page");
    app.handle_key(key(KeyCode::Char(' '), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(current_pdf_page(&app), 3, "Space must turn the page");
}

/// A click on a spreadsheet must leave the arrow keys scrolling the grid, not
/// the file list - the same defect as the PDF, one tab type over.
#[test]
fn a_spreadsheet_opened_with_one_click_scrolls_with_the_arrow_keys() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("data.csv"), csv_body(40)).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    click_open_in_explorer(&mut app, "data.csv");
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        sheet_cursor(&app).0,
        1,
        "Down must act on the grid (cell cursor), not the file list"
    );
}

/// Output scrolling under an on-screen inline image moves only the
/// placement row; re-running the full decode + Lanczos resize + PNG encode
/// per scrolled row froze the render thread for the whole pane height on
/// any real photo.
#[test]
fn a_scrolled_inline_image_reuses_its_baked_payload() {
    let (_tmp, mut app) =
        app_with_baked_terminal_image(crate::iterm2_inline::InlineImageProtocol::Kitty);
    let y0 = app.overlays.terminal_image.layout().unwrap().cell_y;
    let bakes = crate::iterm2_inline::FIT_IMAGE_BAKES.with(|c| c.get());
    app.terminals[0].feed_bytes_for_test(b"\r\n\r\n");
    app.update_terminal_image_overlay();
    let l = app.overlays.terminal_image.layout().unwrap();
    assert_eq!(l.cell_y, y0 - 2, "the placement follows the anchor");
    assert_eq!(
        crate::iterm2_inline::FIT_IMAGE_BAKES.with(|c| c.get()),
        bakes,
        "a pure row move must not re-decode the image"
    );
}

/// A 300-line rendered log (an escape on line 1 makes it one), open in a
/// drawn 100x30 app with the editor focused (#1189).
fn open_rendered_log(tmp: &tempfile::TempDir) -> App {
    let p = tmp.path().join("build.log");
    let mut body = String::from("\u{1b}[32mINFO\u{1b}[0m line 1\n");
    for i in 2..=300 {
        body.push_str(&format!("line {i}\n"));
    }
    std::fs::write(&p, &body).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.open(&p).unwrap();
    assert!(app.editor.log.is_some(), "the fixture is a rendered log");
    draw(&mut app, 100, 30);
    app
}

/// #1189: the arrows, PageUp/PageDown, Home/End and Ctrl+Home/End move a
/// rendered log, which they used to leave on line 1.
#[test]
fn the_keyboard_scrolls_a_rendered_log() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = open_rendered_log(&tmp);
    let press = |app: &mut App, code, mods| app.handle_key(key(code, mods)).unwrap();
    press(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
    let page = app.editor.scroll;
    assert!(page > 10, "PageDown moves a screen: {page}");
    press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.editor.scroll, page + 1);
    press(&mut app, KeyCode::Up, KeyModifiers::NONE);
    press(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
    assert_eq!(app.editor.scroll, 0);
    press(&mut app, KeyCode::End, KeyModifiers::CONTROL);
    let tail = app.editor.scroll;
    assert!(tail > 250, "Ctrl+End reaches the tail: {tail}");
    press(&mut app, KeyCode::Home, KeyModifiers::CONTROL);
    assert_eq!(app.editor.scroll, 0);
    press(&mut app, KeyCode::End, KeyModifiers::NONE);
    assert_eq!(app.editor.scroll, tail, "End is the tail too");
    press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.editor.scroll, tail, "and the tail is where it stops");
}

/// #1189: a key after a wheel scroll carries on from there instead of
/// throwing the view back to line 1.
#[test]
fn a_key_keeps_a_wheel_scrolled_log_where_it_was() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = open_rendered_log(&tmp);
    app.editor.scroll_down(25);
    assert_eq!(app.editor.scroll, 25);
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.scroll, 26);
    app.handle_key(key(KeyCode::Char('j'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.scroll, 26,
        "an unbound key leaves the view alone"
    );
}

/// Negative (#1189): typing into a rendered log changes nothing and never
/// marks it modified, while a plain `.log` with no colour is still an
/// ordinary editable file.
#[test]
fn typing_into_a_rendered_log_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = open_rendered_log(&tmp);
    for code in [
        KeyCode::Char('z'),
        KeyCode::Enter,
        KeyCode::Backspace,
        KeyCode::Tab,
    ] {
        app.handle_key(key(code, KeyModifiers::NONE)).unwrap();
    }
    app.clipboard_reader = || Some(String::from("pasted"));
    app.handle_key(key(KeyCode::Char('v'), KeyModifiers::SUPER))
        .unwrap();
    assert!(!app.editor.dirty, "the read-only log is never modified");
    assert_eq!(app.editor.lines, vec![String::new()]);

    let plain = tmp.path().join("plain.log");
    std::fs::write(&plain, "line 1\nline 2\n").unwrap();
    app.editor.open(&plain).unwrap();
    assert!(app.editor.log.is_none());
    app.handle_key(key(KeyCode::Char('z'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor.dirty, "a plain log is edited as before");
    assert_eq!(app.editor.lines[0], "zline 1");
}

/// #257: Enter and Shift+Enter step through a log's matches. The log has no
/// caret to carry the position, so the walk is anchored on the active match
/// alone; without that anchor every Enter would re-find the same line.
#[test]
fn enter_steps_through_matches_in_a_rendered_log() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("run.log");
    std::fs::write(
        &p,
        "\u{1b}[31mFAIL\u{1b}[0m one\nok\n\u{1b}[31mFAIL\u{1b}[0m two\nok\n",
    )
    .unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.open(&p).unwrap();
    assert!(app.editor.log.is_some());

    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    for ch in "FAIL".chars() {
        app.handle_key(key(KeyCode::Char(ch), KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(
        app.editor_find.as_ref().map(|s| s.match_count),
        Some(2),
        "both FAIL lines are counted through the stripped text"
    );
    assert_eq!(
        app.editor.active_search_match.map(|(r, _, _)| r),
        Some(0),
        "typing lands on the first match"
    );

    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.active_search_match.map(|(r, _, _)| r),
        Some(2),
        "Enter steps to the second match rather than re-finding the first"
    );
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.active_search_match.map(|(r, _, _)| r),
        Some(0),
        "past the last match, Enter wraps to the top"
    );
    app.handle_key(key(KeyCode::Enter, KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(
        app.editor.active_search_match.map(|(r, _, _)| r),
        Some(2),
        "Shift+Enter walks back, wrapping to the last match"
    );
}

/// #257: dragging over a rendered ANSI log selects it, and Cmd+C copies what
/// the reader SEES rather than the escape bytes behind it.
///
/// Driven through the real gestures and a real render, so the body rect the
/// mouse path reads is the one a frame actually painted: a test that set
/// `last_body` by hand would pass with the renderer never having agreed.
#[test]
fn dragging_over_a_rendered_log_selects_and_copies_the_visible_text() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("run.log");
    std::fs::write(
        &p,
        "\u{1b}[32mINFO\u{1b}[0m starting up\n\u{1b}[31mERROR\u{1b}[0m disk full\nbye\n",
    )
    .unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.open(&p).unwrap();
    assert!(app.editor.log.is_some(), "the fixture opens rendered");

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();

    let body = app.editor.log.as_ref().unwrap().last_body;
    assert!(
        body.width > 0 && body.height > 0,
        "the render must have published a body rect for the mouse to hit"
    );

    // Press on the first body row, drag to the second, release.
    use crossterm::event::{MouseButton, MouseEventKind};
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        body.x,
        body.y,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        body.x + 10,
        body.y + 1,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        body.x + 10,
        body.y + 1,
    ));

    let log = app.editor.log.as_ref().unwrap();
    assert!(log.has_selection(), "the drag must leave a selection");
    assert!(!log.dragging, "and the release must end the drag");
    let (text, clamped) = log.selection_text();
    assert!(!clamped);
    assert_eq!(
        text, "INFO starting up\nERROR disk",
        "the copy is the stripped text, not the escapes behind it"
    );

    // What is PAINTED must match what is copied, row for row. Nothing
    // asserted this before: deleting the entire selection-painting block from
    // the renderer left the suite green, so the visible half of the feature
    // had no coverage at all.
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 30,
    });
    term.draw(|frame| app.render(frame)).unwrap();
    {
        use ratatui::widgets::Widget;
        let area = app.editor.last_area;
        (&mut app.editor).render(area, &mut buf);
    }
    // Frame truth: the rect the render that filled THIS buffer published.
    let body = app.editor.log.as_ref().unwrap().last_body;
    let selected_bg = crate::theme::Theme::BLACK.selection();
    let copied_per_row: Vec<usize> = text.split('\n').map(|l| l.chars().count()).collect();
    for (r, expected) in copied_per_row.iter().enumerate() {
        let y = body.y + r as u16;
        let painted = (body.x..body.x + body.width)
            .filter(|x| buf[(*x, y)].style().bg == Some(selected_bg))
            .count();
        assert_eq!(
            painted, *expected,
            "row {r}: {painted} cells painted as selected, {expected} characters copied"
        );
    }

    // Cmd+C puts exactly that on the clipboard.
    app.handle_key(key(KeyCode::Char('c'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        app.status.starts_with("Copied"),
        "the copy must report itself, got {:?}",
        app.status
    );
}

/// #257: dragging past the end of a SHORT line must paint what it copies.
///
/// The renderer painted to the endpoint it was handed while the copy read
/// `min(len)`, and `log_cell_at` clamped the line but not the column, so a
/// drag off the right of a short line painted to the pointer and copied to
/// the line's end. Clamping at the source means both readers see one value
/// instead of each clamping its own way.
#[test]
fn dragging_past_a_short_lines_end_paints_what_it_copies() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("short.log");
    // A coloured line so the view renders, and a very short one to drag off.
    std::fs::write(&p, "\u{1b}[31mabc\u{1b}[0m\nlonger second line here\n").unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.open(&p).unwrap();
    assert!(app.editor.log.is_some());

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let body = app.editor.log.as_ref().unwrap().last_body;

    use crossterm::event::{MouseButton, MouseEventKind};
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        body.x,
        body.y,
    ));
    // Drag far past the end of a three-character line.
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        body.x + 60,
        body.y,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        body.x + 60,
        body.y,
    ));

    let (text, _) = app.editor.log.as_ref().unwrap().selection_text();
    assert_eq!(text, "abc", "the copy stops at the end of the line");

    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 30,
    });
    term.draw(|frame| app.render(frame)).unwrap();
    {
        use ratatui::widgets::Widget;
        let area = app.editor.last_area;
        (&mut app.editor).render(area, &mut buf);
    }
    // Frame truth: the rect to read is the one the render that filled THIS
    // buffer published, not one captured from an earlier frame. Reading the
    // stale rect pointed at the header row and reported nothing painted.
    let body = app.editor.log.as_ref().unwrap().last_body;
    let selected_bg = crate::theme::Theme::BLACK.selection();
    let painted = (body.x..body.x + body.width)
        .filter(|x| buf[(*x, body.y)].style().bg == Some(selected_bg))
        .count();
    assert_eq!(
        painted,
        text.chars().count(),
        "the paint must stop where the copy does, not follow the pointer"
    );
}

/// #404: a double-width character occupies two cells, so every span after
/// it must be painted, and every click after it hit-tested, by display
/// width rather than by character count.
///
/// The painter advanced `x` by `chars().count()`, so after a CJK pair the
/// next span landed two cells to the left of where the terminal had put
/// the pair, overpainting its second half, and `log_cell_at` matched the
/// drift by design. Fixed together so the two never disagree.
#[test]
fn double_width_characters_keep_log_columns_aligned() {
    use crossterm::event::{MouseButton, MouseEventKind};
    use ratatui::widgets::Widget;

    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("wide.log");
    // "\u{4e2d}\u{6587}" is two characters and four cells; then a space and
    // a coloured X, which must land on cell 5 (character column 3).
    std::fs::write(
        &p,
        "\u{1b}[31m\u{4e2d}\u{6587}\u{1b}[0m \u{1b}[32mX\u{1b}[0m\n",
    )
    .unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.open(&p).unwrap();
    assert!(app.editor.log.is_some());

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let body = app.editor.log.as_ref().unwrap().last_body;

    // Click the X and release on it: a one-character selection.
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        body.x + 5,
        body.y,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        body.x + 6,
        body.y,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        body.x + 6,
        body.y,
    ));
    let (text, _) = app.editor.log.as_ref().unwrap().selection_text();
    assert_eq!(text, "X", "cell 5 is the X, two cells past the wide pair");

    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 30,
    });
    term.draw(|frame| app.render(frame)).unwrap();
    {
        let area = app.editor.last_area;
        (&mut app.editor).render(area, &mut buf);
    }
    let body = app.editor.log.as_ref().unwrap().last_body;
    let row: Vec<&str> = (body.x..body.x + 7)
        .map(|x| buf[(x, body.y)].symbol())
        .collect();
    assert_eq!(
        buf[(body.x + 2, body.y)].symbol(),
        "\u{6587}",
        "the second wide character keeps its cells: {row:?}"
    );
    assert_eq!(
        buf[(body.x + 5, body.y)].symbol(),
        "X",
        "the span after the pair starts where the terminal put it: {row:?}"
    );
    let selected_bg = crate::theme::Theme::BLACK.selection();
    let painted: Vec<u16> = (body.x..body.x + body.width)
        .filter(|x| buf[(*x, body.y)].style().bg == Some(selected_bg))
        .collect();
    assert_eq!(
        painted,
        vec![body.x + 5],
        "the selection band sits on the X's cell, not on character column 3"
    );
}

/// #257: a frame that paints no log body must publish no body rect.
///
/// `render_log` returned early for a zero-sized area WITHOUT clearing
/// `last_body`, so after a resize the mouse path could accept a click inside
/// a rectangle this frame had not painted. Frame truth cuts both ways: the
/// rect is a claim about what was drawn, and drawing nothing is a claim too.
#[test]
fn a_log_that_paints_nothing_publishes_no_body_rect() {
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("run.log");
    std::fs::write(&p, "\u{1b}[31mERROR\u{1b}[0m boom\nsecond\n").unwrap();
    let mut e = crate::widgets::editor::Editor::new();
    e.open(&p).unwrap();

    // A real frame publishes a real rect.
    let mut buf = ratatui::buffer::Buffer::empty(Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 8,
    });
    (&mut e).render(
        Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 8,
        },
        &mut buf,
    );
    assert!(
        e.log.as_ref().unwrap().last_body.height > 0,
        "a painted frame publishes its body"
    );

    // A frame with no room paints nothing, so it must claim nothing.
    let mut narrow = ratatui::buffer::Buffer::empty(Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 8,
    });
    (&mut e).render(
        Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        },
        &mut narrow,
    );
    assert_eq!(
        e.log.as_ref().unwrap().last_body,
        Rect::default(),
        "a frame that painted nothing must not leave a stale rect for the \
         mouse path to hit-test against"
    );
}

/// #394: a log past the head budget opens on a partial index, the header
/// says so, and the main loop's `poll_log_index` grows it to the total and
/// re-runs an open find's count once the pass is done.
#[test]
fn a_large_log_opens_before_its_index_is_complete_and_the_loop_finishes_it() {
    let tmp = tempfile::tempdir().unwrap();
    use ratatui::widgets::Widget as _;
    let p = tmp.path().join("huge.log");
    let mut body = Vec::new();
    body.extend_from_slice(b"\x1b[32mINFO\x1b[0m head\n");
    let mut lines = 1usize;
    while (body.len() as u64) < crate::log_view::HEAD_INDEX_BYTES + 1024 * 1024 {
        body.extend_from_slice(format!("line{lines} some padding text\n").as_bytes());
        lines += 1;
    }
    body.extend_from_slice(b"\x1b[31mERROR\x1b[0m disk full\n");
    let total = lines + 1;
    std::fs::write(&p, &body).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.open(&p).unwrap();
    let log = app.editor.log.as_ref().expect("opens as a rendered log");
    assert!(log.indexing(), "the tail is still being indexed after open");
    assert!(log.len() < total, "the count is a lower bound for now");

    let area = Rect::new(0, 0, 80, 12);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut *app.editor).render(area, &mut buf);
    // The header sits on the row above the body the frame published; the
    // editor's own border occupies row 0.
    let body = app.editor.log.as_ref().unwrap().last_body;
    let header: String = (body.x..body.x + body.width)
        .map(|x| buf[(x, body.y - 1)].symbol().to_string())
        .collect();
    assert!(
        header.contains("so far, indexing"),
        "the header must not present a moving count as a total: {header:?}"
    );

    // A find typed while the pass runs: the ERROR line is past the index.
    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    for ch in "disk".chars() {
        app.handle_key(key(KeyCode::Char(ch), KeyModifiers::NONE))
            .unwrap();
    }
    let state = app.editor_find.as_ref().unwrap();
    assert!(
        state.count_truncated(),
        "a count taken against a partial index is flagged partial"
    );

    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(10),
        "the background log index",
        || {
            app.poll_log_index();
            !app.editor.log.as_ref().unwrap().indexing()
        },
    );
    assert_eq!(app.editor.log.as_ref().unwrap().len(), total);
    (&mut *app.editor).render(area, &mut buf);
    // The header sits on the row above the body the frame published; the
    // editor's own border occupies row 0.
    let body = app.editor.log.as_ref().unwrap().last_body;
    let header: String = (body.x..body.x + body.width)
        .map(|x| buf[(x, body.y - 1)].symbol().to_string())
        .collect();
    assert!(
        header.contains(&format!("{total} lines ")) && !header.contains("indexing"),
        "the finished header carries the total: {header:?}"
    );
    assert!(
        !app.poll_log_index(),
        "a finished index asks for no more repaints"
    );
}

/// #1538: a large log viewed at its end and reloaded after an append
/// reopens on a partial index; the main loop's `poll_log_index` settles the
/// view on the new end as the index reaches it, instead of leaving it
/// clamped to the head the open indexed.
#[test]
fn the_loop_brings_a_reloaded_large_log_to_its_new_end() {
    use ratatui::widgets::Widget as _;
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("huge.log");
    let mut body = Vec::new();
    body.extend_from_slice(b"\x1b[32mINFO\x1b[0m head\n");
    let mut lines = 1usize;
    while (body.len() as u64) < crate::log_view::HEAD_INDEX_BYTES + 1024 * 1024 {
        body.extend_from_slice(format!("line{lines} some padding text\n").as_bytes());
        lines += 1;
    }
    std::fs::write(&p, &body).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.open(&p).unwrap();
    let area = Rect::new(0, 0, 80, 12);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut *app.editor).render(area, &mut buf);
    app.editor.log.as_mut().unwrap().finish_index();
    app.editor.scroll_down(usize::MAX / 2);
    let rows = (app.editor.last_inner.height as usize).saturating_sub(1);
    assert_eq!(
        app.editor.scroll + rows,
        lines,
        "the view is on the last line"
    );

    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, "\x1b[31mERROR\x1b[0m disk full").unwrap();
    }
    app.editor.reload_or_flag_conflict();
    assert!(app.editor.log.as_ref().unwrap().indexing());
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(10),
        "the background log index",
        || {
            app.poll_log_index();
            !app.editor.log.as_ref().unwrap().indexing()
        },
    );
    assert_eq!(
        app.editor.scroll + rows,
        lines + 1,
        "the view follows the log to its new last line"
    );
}
/// #370: a `.http` file runs the request under the caret. The variables
/// substitute from `.http.env.json` on the WIRE only: the status line,
/// history entry, and response tab all carry the raw `{{name}}` form, so
/// the token never leaves the env file. A JSON response opens pretty-printed
/// as `.jsonc`; a PNG response opens through the image path.
#[test]
fn an_http_file_runs_requests_and_keeps_secrets_out_of_history_and_the_tab() {
    use std::io::{Read as _, Write as _};
    let tmp = tempfile::tempdir().unwrap();
    // The cache-dir override is a process-wide global shared with the relay
    // family: hold their lock and restore via RAII, exactly as
    // `with_relay_home` does, so a panic cannot leak the redirect into
    // later tests (#428 review).
    struct RestoreCacheDir;
    impl Drop for RestoreCacheDir {
        fn drop(&mut self) {
            *CACHE_DIR_OVERRIDE_FOR_TEST.lock().unwrap() = None;
        }
    }
    let _cache_lock = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let _restore = RestoreCacheDir;
    *CACHE_DIR_OVERRIDE_FOR_TEST.lock().unwrap() = Some(tmp.path().join("cache"));

    // A 1x1 PNG via the image crate, so the decode on open genuinely works.
    let png_bytes = {
        let img: image::RgbaImage = image::ImageBuffer::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
        let mut buf: Vec<u8> = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    };

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen2 = seen.clone();
    let png_served = png_bytes.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { break };
            let mut buf = vec![0u8; 8192];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let (body, ct): (Vec<u8>, &str) = if req.starts_with("GET /users") {
                (
                    b"{\"users\":[{\"id\":1,\"name\":\"ada\"}]}".to_vec(),
                    "application/json",
                )
            } else {
                (png_served.clone(), "image/png")
            };
            seen2.lock().unwrap().push(req);
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {ct}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(&body);
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    });

    let http = tmp.path().join("api.http");
    std::fs::write(
        &http,
        "GET {{host}}/users\nAuthorization: Bearer {{token}}\n\n###\nGET {{host}}/logo.png\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join(crate::http_file::ENV_FILE),
        format!("{{\"host\": \"http://127.0.0.1:{port}\", \"token\": \"super-secret-token\"}}"),
    )
    .unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&http).unwrap();
    app.editor.cursor_row = 1;
    app.send_http_request_under_caret();
    assert!(
        app.status.starts_with("Sending GET {{host}}/users"),
        "the status carries the RAW line: {}",
        app.status
    );
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(10),
        "the HTTP response",
        || {
            app.drain_http_responses();
            app.http_run.is_none()
        },
    );
    let opened = app.editor.path.clone().expect("a response tab opened");
    assert_eq!(opened.extension().and_then(|e| e.to_str()), Some("jsonc"));
    let doc = app.editor.lines.join("\n");
    assert!(
        doc.contains("// GET {{host}}/users \u{2192} HTTP/1.1 200 OK"),
        "{doc}"
    );
    assert!(doc.contains("\"users\": ["), "pretty-printed: {doc}");
    assert!(
        !doc.contains("super-secret-token"),
        "the token never reaches the tab"
    );
    assert!(
        seen.lock().unwrap()[0].contains("Bearer super-secret-token"),
        "but the wire DID carry the substituted value"
    );
    let hits = app.command_history.search(
        "http GET",
        crate::command_history::HistoryScope::All,
        "",
        "",
    );
    assert_eq!(
        hits[0].cmd, "http GET {{host}}/users",
        "history keeps the raw line"
    );
    assert_eq!(hits[0].exit, Some(200));
    assert!(!hits[0].cmd.contains("super-secret-token"));

    // The PNG block renders through the image path.
    app.editor.open_pinned(&http).unwrap();
    app.editor.cursor_row = 4;
    app.send_http_request_under_caret();
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(10),
        "the PNG response",
        || {
            app.drain_http_responses();
            app.http_run.is_none()
        },
    );
    assert!(
        app.editor.image.is_some(),
        "a PNG response opens as an image, not bytes in a text tab: {:?}",
        app.editor.path
    );

    // A second send while one is in flight is refused and clobbers
    // nothing; a worker that died reports itself on the next drain.
    let (dummy_tx, dummy_rx) = std::sync::mpsc::channel();
    app.http_run = Some(dummy_rx);
    app.editor.open_pinned(&http).unwrap();
    app.editor.cursor_row = 0;
    app.send_http_request_under_caret();
    assert_eq!(app.status, "An HTTP request is already in flight");
    drop(dummy_tx);
    assert!(app.drain_http_responses());
    assert!(app.status.contains("worker died"), "{}", app.status);
    assert!(app.http_run.is_none());

    // A transport failure must NOT leak a substituted secret into the
    // status line: ureq's error Display opens with the resolved URL, so
    // only its kind may be shown (#428 review). The port is bound then
    // dropped, so the connection is refused at once.
    let dead_port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    std::fs::write(
        tmp.path().join(crate::http_file::ENV_FILE),
        format!(
            "{{\"host\": \"http://127.0.0.1:{dead_port}\", \"token\": \"super-secret-token\"}}"
        ),
    )
    .unwrap();
    let secret_url = tmp.path().join("leak.http");
    std::fs::write(&secret_url, "GET {{host}}/x?api_key={{token}}\n").unwrap();
    app.editor.open_pinned(&secret_url).unwrap();
    app.editor.cursor_row = 0;
    app.send_http_request_under_caret();
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(10),
        "the refused connection",
        || {
            app.drain_http_responses();
            app.http_run.is_none()
        },
    );
    assert!(
        app.status.contains("failed"),
        "a transport failure is reported: {}",
        app.status
    );
    assert!(
        !app.status.contains("super-secret-token"),
        "the substituted URL never reaches the status line: {}",
        app.status
    );
    let hits =
        app.command_history
            .search("api_key", crate::command_history::HistoryScope::All, "", "");
    assert_eq!(hits[0].exit, None, "a failed run records no status code");
    assert!(!hits[0].cmd.contains("super-secret-token"));
    app.editor.open_pinned(&http).unwrap();

    // A hole with no value refuses to send rather than leaking the hole.
    app.editor.open_pinned(&http).unwrap();
    std::fs::remove_file(tmp.path().join(crate::http_file::ENV_FILE)).unwrap();
    app.editor.cursor_row = 0;
    app.send_http_request_under_caret();
    assert!(
        app.status.contains("no value for {{host}}, {{token}}"),
        "{}",
        app.status
    );
    assert!(app.http_run.is_none(), "nothing was sent");
}
/// #1186: a `.http` file's own `@base = …` line fills `{{base}}`, and
/// Ctrl+Enter on the `@base` line sends nothing.
#[test]
fn an_http_file_variable_fills_the_request_and_is_not_sent() {
    let tmp = tempfile::tempdir().unwrap();
    let http = tmp.path().join("api.http");
    std::fs::write(
        &http,
        "@base = http://127.0.0.1:8765\n\n### Fetch the hello document\nGET {{base}}/hello.json\nAccept: application/json\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&http).unwrap();
    app.editor.cursor_row = 3;
    let (_, resolved, _) = app.http_request_under_caret().expect("resolves");
    assert_eq!(resolved.url, "http://127.0.0.1:8765/hello.json");

    app.editor.cursor_row = 0;
    app.send_http_request_under_caret();
    assert_eq!(app.status, "No request under the caret");
    assert!(app.http_run.is_none(), "the @base line is not sent");
}

/// Negative (#1186): a hole neither the file nor `.http.env.json` defines
/// still refuses to send, and the status says where a value can go.
#[test]
fn an_http_hole_no_file_variable_defines_still_refuses() {
    let tmp = tempfile::tempdir().unwrap();
    let http = tmp.path().join("api.http");
    std::fs::write(
        &http,
        "@base = http://127.0.0.1:8765\nGET {{base}}/{{path}}\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&http).unwrap();
    app.editor.cursor_row = 1;
    app.send_http_request_under_caret();
    assert!(
        app.status.contains("no value for {{path}}"),
        "{}",
        app.status
    );
    assert!(!app.status.contains("{{base}}"), "{}", app.status);
    assert!(app.status.contains("@name"), "{}", app.status);
    assert!(app.http_run.is_none(), "nothing was sent");
}

/// #1537: "File: Reopen as Preview" on a plain text capture opens the
/// rendered log view instead of refusing, keeps it through a reload, and
/// "Reopen as Text" goes back to the editable buffer.
#[test]
fn reopen_as_preview_shows_plain_text_as_a_rendered_log() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("run.out");
    std::fs::write(&out, "2026-10-08 ERROR boom\nINFO ok\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&out).unwrap();
    assert!(app.editor.log.is_none(), "plain text opens as text");

    app.run_command(Command::ReopenAsPreview);
    assert!(
        app.editor.log.is_some(),
        "the capture is shown as a rendered log; status {:?}",
        app.status
    );
    assert!(!app.status.contains("No preview"), "{:?}", app.status);

    std::fs::write(&out, "2026-10-08 ERROR boom\nINFO ok\nWARN late\n").unwrap();
    assert!(app.editor.reload_if_clean().is_some_and(|r| r.is_ok()));
    assert!(app.editor.log.is_some(), "a reload keeps the preview");

    app.run_command(Command::ReopenAsText);
    assert!(app.editor.log.is_none(), "back to the text");
    assert_eq!(app.editor.lines[2], "WARN late");

    let other = tmp.path().join("other.out");
    std::fs::write(&other, "plain\n").unwrap();
    app.editor.open_pinned(&out).unwrap();
    app.run_command(Command::ReopenAsPreview);
    app.editor.open(&other).unwrap();
    assert!(
        app.editor.log.is_none(),
        "the choice belonged to run.out, not to the next file"
    );
}

/// #1537 negative: the log view reads raw bytes as UTF-8, so a UTF-16
/// capture stays in the decoded text buffer instead of turning to NULs.
#[test]
fn reopen_as_preview_keeps_a_utf16_capture_as_text() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("run.out");
    let mut bytes = vec![0xFF, 0xFE];
    for u in "ERROR boom\nINFO ok\n".encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    std::fs::write(&out, bytes).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&out).unwrap();
    assert_eq!(app.editor.encoding, encoding_rs::UTF_16LE);
    app.run_command(Command::ReopenAsPreview);
    assert!(app.editor.log.is_none(), "no byte-level log view of UTF-16");
    assert_eq!(app.editor.lines[0], "ERROR boom");
    assert_eq!(
        app.status,
        "No preview for this file type; showing the text"
    );
}

/// #1537 negative: source code has a grammar and no preview, so Reopen as
/// Preview still says so rather than turning it into a log.
#[test]
fn reopen_as_preview_leaves_source_code_as_text() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("main.rs");
    std::fs::write(&src, "fn main() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&src).unwrap();
    app.run_command(Command::ReopenAsPreview);
    assert!(app.editor.log.is_none());
    assert_eq!(
        app.status,
        "No preview for this file type; showing the text"
    );
}

/// #1537: the "Log: Highlighting (tailspin)" switch reaches a plain `.log`
/// already open as text, not only rendered log views.
#[test]
fn the_log_highlight_switch_recolours_an_open_plain_log() {
    let _exclusive = crate::log_view::DEFAULT_HIGHLIGHT_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let before = crate::log_view::default_highlight();
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("app.log");
    std::fs::write(&log, "ERROR boom\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_log_highlight(true);
    app.editor.open_pinned(&log).unwrap();
    assert!(app.editor.log.is_none(), "a plain log is a text tab");
    let coloured = |app: &App| !app.editor.highlights_for_test()[0].is_empty();
    assert!(coloured(&app), "ERROR is coloured");
    app.set_log_highlight(false);
    assert!(!coloured(&app), "switched off, the open tab goes plain");
    app.set_log_highlight(true);
    assert!(coloured(&app), "and back on, coloured again");
    crate::log_view::set_default_highlight(before);
}

#[test]
fn toggling_log_highlighting_flips_open_views_and_the_default() {
    // #466: the palette / Settings toggle reaches the view that is already
    // open AND steers views opened afterwards, so the user sees the change
    // at once and does not get it undone by the next log they open.
    let _exclusive = crate::log_view::DEFAULT_HIGHLIGHT_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("build.log");
    std::fs::write(&log, b"\x1b[32mok\x1b[0m 2024-01-02 step 1\nplain step 2\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&log).unwrap();
    assert!(
        app.editor.log.is_some(),
        "a coloured .log opens as a rendered log"
    );
    let before = app.log_highlight;
    let view_before = app.editor.log.as_ref().unwrap().highlight();
    assert_eq!(
        view_before, before,
        "the open view starts at the app's setting"
    );

    app.toggle_log_highlight();
    assert_eq!(app.log_highlight, !before);
    assert_eq!(
        app.editor.log.as_ref().unwrap().highlight(),
        !before,
        "the open view followed the toggle"
    );
    assert_eq!(
        crate::log_view::default_highlight(),
        !before,
        "and so will the next log opened"
    );
    assert!(app.status.starts_with("Log highlighting (tailspin):"));

    // Back to where it was, so the process-wide default is left as found.
    app.toggle_log_highlight();
    assert_eq!(app.log_highlight, before);
    assert_eq!(crate::log_view::default_highlight(), before);
}

#[test]
fn startup_seeds_the_log_view_default_from_the_saved_preference() {
    // #466: `App::new` seeds the log view's opening default from the saved
    // preference. Under test that seed is recorded per thread instead of
    // written (hundreds of apps on parallel threads would race the tests
    // reading the process-wide default), which keeps the CALL SITE
    // observable: this asserts what `App::new` decided, pinned by a
    // workspace layer so it does not move with the developer's own config.
    let tmp = tempfile::tempdir().unwrap();
    let layer = crate::config_layers::workspace_config_path(tmp.path());
    std::fs::create_dir_all(layer.parent().unwrap()).unwrap();
    std::fs::write(&layer, r#"{"disable_log_highlight": true}"#).unwrap();
    STARTUP_LOG_HIGHLIGHT_SEED.with(|c| c.set(None));
    let app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(
        !app.log_highlight,
        "the workspace layer's opt-out reached the field"
    );
    assert_eq!(
        STARTUP_LOG_HIGHLIGHT_SEED.with(|c| c.get()),
        Some(false),
        "and App::new seeded the log view's opening default with the same value"
    );
    std::fs::write(&layer, r#"{"disable_log_highlight": false}"#).unwrap();
    STARTUP_LOG_HIGHLIGHT_SEED.with(|c| c.set(None));
    let app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(app.log_highlight);
    assert_eq!(STARTUP_LOG_HIGHLIGHT_SEED.with(|c| c.get()), Some(true));
}

/// #682: the fingerprint under an image changes with any write ratatui
/// makes inside it, and ignores writes outside it.
#[test]
fn the_cells_under_an_image_fingerprint_writes_inside_it_only() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 10));
    let before = super::cells_fingerprint(&buf, 2, 2, 5, 3);
    buf[(15, 8)].set_symbol("x");
    assert_eq!(super::cells_fingerprint(&buf, 2, 2, 5, 3), before);
    buf[(3, 3)].set_symbol("x");
    assert_ne!(super::cells_fingerprint(&buf, 2, 2, 5, 3), before);
    // A rectangle past the edge is clamped, not a panic.
    let _ = super::cells_fingerprint(&buf, 18, 8, 10, 10);
}

/// On Kitty an image lives on its own layer, so text redrawn beneath it
/// never makes croft resend it; on iTerm2 it does.
#[test]
fn only_cell_buffer_protocols_resend_an_image_when_text_beneath_changes() {
    use crate::iterm2_inline::InlineImageProtocol;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.terminal_image.set(
        String::from("img"),
        super::TerminalImageLayout {
            cell_x: 1,
            cell_y: 1,
            cell_w: 4,
            cell_h: 2,
            seq: 0,
            pane: 0,
        },
    );
    let quiet = Buffer::empty(Rect::new(0, 0, 10, 5));
    let mut busy = quiet.clone();
    busy[(2, 1)].set_symbol("x");
    app.inline_protocol = InlineImageProtocol::ITerm2;
    assert_ne!(
        app.image_underlays(&quiet).terminal,
        app.image_underlays(&busy).terminal
    );
    app.inline_protocol = InlineImageProtocol::Kitty;
    assert_eq!(
        app.image_underlays(&quiet).terminal,
        app.image_underlays(&busy).terminal
    );
}

/// #682: only iTerm2 needs the idle icon keepalive.
#[test]
fn only_iterm2_keeps_the_activity_icons_alive() {
    use crate::iterm2_inline::InlineImageProtocol;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for (p, want) in [
        (InlineImageProtocol::ITerm2, true),
        (InlineImageProtocol::Kitty, false),
        (InlineImageProtocol::Sixel, false),
    ] {
        app.inline_protocol = p;
        assert_eq!(app.activity_keepalive_allowed(), want, "{p:?}");
    }
}

#[test]
fn a_csv_sheet_sorts_by_the_cursors_column_from_the_palette() {
    // #578: raw results as a sortable table. Any CSV sheet sorts.
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let csv = tmp.path().join("results.csv");
    std::fs::write(&csv, "name,count\nb,10\nA,9\nc,100\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_command(Command::SheetSortByColumn);
    assert_eq!(app.status, "Open a CSV or TSV file to sort it");
    app.editor.open(&csv).unwrap();
    let sheet = app.editor.sheet.as_mut().expect("a CSV opens as a sheet");
    sheet.sheets[0].cur_col = 1;
    app.run_command(Command::SheetSortByColumn);
    assert_eq!(app.status, "Sorted by count, ascending");
    let rows = |app: &App| -> Vec<String> {
        app.editor.sheet.as_ref().unwrap().sheets[0]
            .rows
            .iter()
            .map(|r| r[1].clone())
            .collect()
    };
    assert_eq!(rows(&app), ["9", "10", "100"]);
    assert!(app.editor.dirty, "a reorder is an edit");
    app.run_command(Command::SheetSortByColumn);
    assert_eq!(app.status, "Sorted by count, descending");
    assert_eq!(rows(&app), ["100", "10", "9"]);
}

/// A WAL-mode database with one row, and the connection that wrote it,
/// kept open like a running app's so nothing checkpoints into the file.
fn wal_database(dir: &std::path::Path) -> (std::path::PathBuf, rusqlite::Connection) {
    let db = dir.join("app.db");
    let writer = rusqlite::Connection::open(&db).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; \
             CREATE TABLE kv (k TEXT); INSERT INTO kv VALUES ('a');",
        )
        .unwrap();
    (db, writer)
}

fn sqlite_keys(app: &App) -> Vec<String> {
    let data = &app.editor.sheet.as_ref().expect("a SQLite tab").sheets[0];
    data.rows.iter().map(|r| r[0].clone()).collect()
}

/// #1643: a commit from an app holding its WAL-mode connection open lands
/// in `app.db-wal` and leaves `app.db` alone; the open database tab still
/// picks it up on the next sweep.
#[test]
fn a_wal_commit_by_a_running_app_reloads_the_database_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, writer) = wal_database(tmp.path());
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&db).unwrap();
    assert_eq!(sqlite_keys(&app), ["a"]);
    let stamp = |p: &std::path::Path| {
        let m = std::fs::metadata(p).unwrap();
        (m.modified().unwrap(), m.len())
    };
    let before = stamp(&db);
    writer.execute("INSERT INTO kv VALUES ('b')", []).unwrap();
    assert_eq!(stamp(&db), before, "setup: the commit left app.db alone");
    assert!(app.reload_open_file_after_external_change());
    assert_eq!(sqlite_keys(&app), ["a", "b"]);
    drop(writer);
}

/// #1643: the poll stats a SQLite tab's write-ahead log as well as the
/// database, even before the log exists, and no log for any other tab.
#[test]
fn the_poll_watches_a_sqlite_tabs_log_and_no_other_tabs() {
    let tmp = tempfile::tempdir().unwrap();
    let db = sqlite_table_of(tmp.path(), 3);
    let txt = tmp.path().join("notes.txt");
    std::fs::write(&txt, "x\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&txt).unwrap();
    app.editor.open_in_new_tab(&db).unwrap();
    let polled = app.polled_open_file_paths();
    let real_dir = std::fs::canonicalize(tmp.path()).unwrap();
    assert!(polled.contains(&real_dir.join("big.db-wal")), "{polled:?}");
    assert!(
        !polled.contains(&tmp.path().join("notes.txt-wal"))
            && !polled.contains(&real_dir.join("notes.txt-wal")),
        "{polled:?}"
    );
    assert!(polled.contains(&db) && polled.contains(&txt));
}

/// Negative (#1643): with no commit, the sweep leaves the database tab
/// alone, cell cursor included, even while a writer holds it open.
#[test]
fn a_wal_database_with_no_new_commit_is_not_reloaded() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, writer) = wal_database(tmp.path());
    writer.execute("INSERT INTO kv VALUES ('b')", []).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&db).unwrap();
    app.editor.sheet.as_mut().unwrap().sheets[0].cur_row = 1;
    assert!(!app.reload_open_file_after_external_change());
    assert_eq!(app.editor.sheet.as_ref().unwrap().sheets[0].cur_row, 1);
    assert_eq!(sqlite_keys(&app), ["a", "b"]);
    drop(writer);
}

/// #1643: a database opened through a symlink has its write-ahead log
/// beside the real file, and a commit there still reloads the tab.
#[cfg(unix)]
#[test]
fn a_wal_commit_reloads_a_database_opened_through_a_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let (db, writer) = wal_database(&real);
    let link = tmp.path().join("link.db");
    std::os::unix::fs::symlink(&db, &link).unwrap();
    assert_eq!(
        crate::widgets::editor::sqlite_wal_path(&link),
        Some(std::fs::canonicalize(&real).unwrap().join("app.db-wal"))
    );
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&link).unwrap();
    assert_eq!(sqlite_keys(&app), ["a"]);
    writer.execute("INSERT INTO kv VALUES ('b')", []).unwrap();
    assert!(app.reload_open_file_after_external_change());
    assert_eq!(sqlite_keys(&app), ["a", "b"]);
    drop(writer);
}

/// #1643: a WAL commit's reload keeps the reader's table, page, cell and
/// scroll, and clamps them when the page no longer exists.
#[test]
fn a_wal_reload_keeps_the_sqlite_table_page_and_cell() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("app.db");
    let writer = rusqlite::Connection::open(&db).unwrap();
    writer
        .execute_batch(&format!(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; \
             CREATE TABLE a_small (k TEXT); INSERT INTO a_small VALUES ('a'); \
             CREATE TABLE z_big (n INTEGER, m INTEGER); WITH RECURSIVE c(x) AS \
             (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < {}) \
             INSERT INTO z_big SELECT x, x FROM c;",
            crate::sqlite_view::ROW_CAP + 40
        ))
        .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&db).unwrap();
    app.editor.sheet.as_mut().unwrap().current_sheet = 1;
    app.sqlite_load_page(1, false);
    {
        let data = &mut app.editor.sheet.as_mut().unwrap().sheets[1];
        (data.cur_row, data.cur_col) = (30, 1);
        (data.scroll_row, data.scroll_col) = (20, 1);
    }
    writer
        .execute("INSERT INTO a_small VALUES ('b')", [])
        .unwrap();
    assert!(app.reload_open_file_after_external_change());
    let view = app.editor.sheet.as_ref().unwrap();
    assert_eq!(view.current_sheet, 1);
    assert_eq!(view.sqlite_pages[1], (String::from("z_big"), 1));
    let data = &view.sheets[1];
    assert_eq!(data.row_base, crate::sqlite_view::ROW_CAP);
    assert_eq!((data.cur_row, data.cur_col), (30, 1));
    assert_eq!((data.scroll_row, data.scroll_col), (20, 1));
    assert_eq!(view.sheets[0].rows.len(), 2, "the reload saw the commit");

    // The table shrinks below that page: back to its first, cell clamped.
    writer.execute("DELETE FROM z_big WHERE n > 5", []).unwrap();
    assert!(app.reload_open_file_after_external_change());
    let view = app.editor.sheet.as_ref().unwrap();
    assert_eq!(view.current_sheet, 1);
    assert_eq!(view.sqlite_pages[1].1, 0);
    let data = &view.sheets[1];
    assert_eq!((data.cur_row, data.cur_col), (4, 1));
    assert!(data.scroll_row <= data.cur_row);
    drop(writer);
}
