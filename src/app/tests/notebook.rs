//! Jupyter notebooks and their kernels.

use super::*;

#[test]
fn notebook_opens_rendered_with_reopen_as_text_for_the_json() {
    // #180: an .ipynb opens as the rendered view over its JSON text;
    // Reopen as Text shows the raw JSON and sticks.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("run.ipynb");
    std::fs::write(
        &p,
        r##"{"nbformat":4,"metadata":{"kernelspec":{"language":"python"}},"cells":[
            {"cell_type":"markdown","source":["# Report"]},
            {"cell_type":"code","execution_count":3,"source":["x = 1"],
             "outputs":[{"output_type":"stream","text":["done\n"]}]}]}"##,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    let md = app
        .editor
        .markdown_preview
        .as_ref()
        .expect("notebook opens rendered");
    assert!(md.notebook);
    let all: String = md
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert!(all.contains("Report"));
    assert!(all.contains("In [3]:"));
    assert!(all.contains("done"));

    // Reopen as Text: the raw JSON, sticky across a same-path reload.
    app.editor.force_text = true;
    app.editor.open(&p).unwrap();
    assert!(app.editor.markdown_preview.is_none());
    assert!(app.editor.lines[0].contains("nbformat"));
    app.editor.open(&p).unwrap();
    assert!(app.editor.markdown_preview.is_none(), "sticky");
    // And back to the render.
    app.editor.force_text = false;
    app.editor.open(&p).unwrap();
    assert!(app.editor.markdown_preview.is_some());
}

const NOTEBOOK_355: &str = "{\n \"cells\": [\n  {\n   \"cell_type\": \"code\",\n   \"execution_count\": null,\n   \"id\": \"c1\",\n   \"metadata\": {},\n   \"outputs\": [],\n   \"source\": [\n    \"print(1+1)\"\n   ]\n  }\n ],\n \"metadata\": {\n  \"kernelspec\": {\n   \"display_name\": \"Python 3\",\n   \"language\": \"python\",\n   \"name\": \"python3\"\n  }\n },\n \"nbformat\": 4,\n \"nbformat_minor\": 5\n}\n";

/// Click a notebook cell's run glyph where it is drawn.
fn click_first_cell_glyph(
    app: &mut App,
    term: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
) {
    term.draw(|f| app.render(f)).unwrap();
    let md = app
        .editor
        .markdown_preview
        .as_ref()
        .expect("notebook preview open");
    let (x, y) = (
        md.last_area.x,
        md.last_area.y + (md.run_rows[0] - md.scroll as usize) as u16,
    );
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

/// A notebook cell's run glyph, end to end through the App with the test
/// standing in for the kernel: the click sends the cell's code, the cell
/// shows `In [*]` while it runs, the kernel coming up clears the old
/// output, and the kernel's output and count land in the buffer as an edit.
#[test]
fn clicking_a_cells_run_glyph_runs_it_and_writes_the_output_into_the_notebook() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("t.ipynb");
    // A previous run's output, which this run must replace.
    let stale = NOTEBOOK_355.replace(
        "\"outputs\": [],",
        "\"outputs\": [{\"output_type\": \"stream\", \"name\": \"stdout\", \"text\": [\"stale\\n\"]}],",
    );
    assert_ne!(stale, NOTEBOOK_355);
    std::fs::write(&path, stale).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&path).unwrap();
    let (run, kernel, requests) = crate::notebook_kernel::NotebookRun::for_test();
    app.notebook_kernels.insert(path.clone(), run);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    click_first_cell_glyph(&mut app, &mut term);

    let sent: Vec<_> = requests.try_iter().collect();
    let id = match sent.as_slice() {
        [crate::notebook_kernel::Request::Execute { id, code }] => {
            assert_eq!(code, "print(1+1)");
            id.clone()
        }
        other => panic!("expected one execute, got {other:?}"),
    };
    assert_eq!(app.editor.notebook_running, vec![0]);
    assert!(
        app.editor.lines.join("\n").contains("stale"),
        "#878: the old output stays until the kernel is up"
    );
    term.draw(|f| app.render(f)).unwrap();
    let md = app.editor.markdown_preview.as_ref().unwrap();
    let frame = &md.lines[md.runnables[0].first_line];
    assert_eq!(frame.spans[1].content.as_ref(), "In [*]:");

    kernel
        .send(crate::notebook_kernel::Event::Ready {
            kernel: "python3".into(),
            display: "Python 3".into(),
        })
        .unwrap();
    kernel
        .send(crate::notebook_kernel::Event::Output {
            id: id.clone(),
            output: serde_json::json!({"output_type": "stream", "name": "stdout", "text": "2\n"}),
        })
        .unwrap();
    kernel
        .send(crate::notebook_kernel::Event::Done {
            id,
            execution_count: Some(1),
            status: "ok".into(),
        })
        .unwrap();
    assert!(app.poll_notebook_kernels());
    let doc: serde_json::Value = serde_json::from_str(&app.editor.lines.join("\n")).unwrap();
    assert_eq!(doc["cells"][0]["execution_count"], 1);
    assert_eq!(
        doc["cells"][0]["outputs"],
        serde_json::json!([{"output_type": "stream", "name": "stdout", "text": ["2\n"]}]),
        "the run's output replaced the old one"
    );
    assert!(app.editor.notebook_running.is_empty());
    assert!(app.editor.dirty, "outputs are an edit; Save writes them");
}

/// The kernel bridges a test App starts through Select Kernel (#1377), by
/// kernel name, with the test speaking for each.
type StartedKernels = std::rc::Rc<
    std::cell::RefCell<
        Vec<(
            String,
            std::sync::mpsc::Sender<crate::notebook_kernel::Event>,
            std::sync::mpsc::Receiver<crate::notebook_kernel::Request>,
        )>,
    >,
>;

/// A notebook naming `kernel` open in its rendered view, with the test as
/// its kernel and as every kernel started after it.
fn notebook_with_kernel(
    kernel: &str,
) -> (
    tempfile::TempDir,
    App,
    PathBuf,
    std::sync::mpsc::Sender<crate::notebook_kernel::Event>,
    StartedKernels,
) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("shared.ipynb");
    std::fs::write(
        &path,
        NOTEBOOK_355.replace("\"name\": \"python3\"", &format!("\"name\": \"{kernel}\"")),
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&path).unwrap();
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let (run, kernel_tx, _requests) = crate::notebook_kernel::NotebookRun::for_test();
    app.notebook_kernels.insert(path.clone(), run);
    let started: StartedKernels = Default::default();
    let record = started.clone();
    app.kernel_start_override = Some(Box::new(move |name| {
        let (session, events, requests) = crate::notebook_kernel::KernelSession::for_test();
        record
            .borrow_mut()
            .push((name.to_string(), events, requests));
        session
    }));
    (tmp, app, path, kernel_tx, started)
}

fn installed_kernels() -> Vec<crate::notebook_kernel::KernelSpec> {
    vec![
        crate::notebook_kernel::KernelSpec {
            name: "ir".into(),
            display: "R".into(),
            language: "R".into(),
        },
        crate::notebook_kernel::KernelSpec {
            name: "python3".into(),
            display: "Python 3 (ipykernel)".into(),
            language: "python".into(),
        },
    ]
}

fn notebook_kernelspec(app: &App) -> serde_json::Value {
    let doc: serde_json::Value = serde_json::from_str(&app.editor.lines.join("\n")).unwrap();
    doc["metadata"]["kernelspec"].clone()
}

/// #1377: Run All on a notebook naming a kernel this machine does not have
/// asks which installed kernel to use, opened on the one for the
/// notebook's language; choosing it runs the cells and names it in the
/// notebook, as Jupyter does.
#[test]
fn run_all_with_a_missing_kernel_asks_which_kernel_and_runs_on_it() {
    use crate::notebook_kernel::{Event, Request};
    use crate::widgets::list_picker::ListPurpose;
    let (_tmp, mut app, _path, kernel, started) = notebook_with_kernel("conda-env-ds-py");
    app.run_command(crate::widgets::command_palette::Command::NotebookRunAll);
    assert_eq!(app.editor.notebook_running, vec![0]);
    kernel
        .send(Event::Missing {
            kernel: "conda-env-ds-py".into(),
            specs: installed_kernels(),
        })
        .unwrap();
    app.poll_notebook_kernels();
    let picker = app.list_picker.as_ref().expect("the kernel picker opens");
    assert_eq!(picker.purpose, ListPurpose::NotebookKernel);
    assert!(picker.title.contains("conda-env-ds-py"), "{}", picker.title);
    assert_eq!(
        picker.selected_row().map(|r| r.id.as_str()),
        Some("python3"),
        "the Python notebook's picker opens on the Python kernel"
    );
    assert_eq!(app.editor.notebook_running, vec![0], "the cell waits");

    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.list_picker.is_none());
    let started = started.borrow();
    let [(name, _, requests)] = started.as_slice() else {
        panic!("one kernel started: {:?}", started.len());
    };
    assert_eq!(name, "python3");
    match requests.try_iter().collect::<Vec<_>>().as_slice() {
        [Request::Execute { code, .. }] => assert_eq!(code, "print(1+1)"),
        other => panic!("the waiting cell goes to the chosen kernel: {other:?}"),
    }
    let ks = notebook_kernelspec(&app);
    assert_eq!(ks["name"], "python3");
    assert_eq!(ks["display_name"], "Python 3 (ipykernel)");
    assert!(app.editor.dirty, "Save writes the new kernel into the file");
    assert_eq!(app.status, "Starting kernel Python 3 (ipykernel)…");
}

/// #1377, negative: closing the picker without a choice leaves nothing
/// waiting at `In [*]` and the notebook's kernel as it was.
#[test]
fn declining_the_kernel_picker_stops_the_waiting_cells() {
    use crate::notebook_kernel::Event;
    let (_tmp, mut app, path, kernel, started) = notebook_with_kernel("conda-env-ds-py");
    app.run_command(crate::widgets::command_palette::Command::NotebookRunAll);
    kernel
        .send(Event::Missing {
            kernel: "conda-env-ds-py".into(),
            specs: installed_kernels(),
        })
        .unwrap();
    app.poll_notebook_kernels();
    assert!(app.list_picker.is_some());
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.list_picker.is_none());
    assert!(app.editor.notebook_running.is_empty());
    assert!(!app.notebook_kernels.contains_key(&path));
    assert!(started.borrow().is_empty(), "no kernel was started");
    assert_eq!(notebook_kernelspec(&app)["name"], "conda-env-ds-py");
    assert!(
        app.status.contains("Notebook: Select Kernel"),
        "{}",
        app.status
    );
}

/// #1377, negative: a kernel that is installed starts with no question.
#[test]
fn a_kernel_that_is_installed_opens_no_picker() {
    use crate::notebook_kernel::Event;
    let (_tmp, mut app, _path, kernel, started) = notebook_with_kernel("python3");
    app.run_command(crate::widgets::command_palette::Command::NotebookRunAll);
    kernel
        .send(Event::Ready {
            kernel: "python3".into(),
            display: "Python 3 (ipykernel)".into(),
        })
        .unwrap();
    app.poll_notebook_kernels();
    assert!(app.list_picker.is_none());
    assert!(started.borrow().is_empty());
    assert_eq!(app.editor.notebook_running, vec![0]);
}

/// The `In [..]` label the rendered view draws on the notebook's first cell.
fn first_cell_label(
    app: &mut App,
    term: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
) -> String {
    term.draw(|f| app.render(f)).unwrap();
    let md = app.editor.markdown_preview.as_ref().unwrap();
    md.lines[md.runnables[0].first_line].spans[1]
        .content
        .to_string()
}

/// #878: Run All with no Python that has ipykernel leaves the notebook
/// byte-identical and clean, with nothing left at `In [*]`, and once
/// ipykernel is installed the next Run All starts a new kernel instead of
/// sending its cells to the one that never started.
#[test]
fn a_kernel_that_fails_to_start_leaves_the_notebook_as_it_was_and_the_next_run_starts_afresh() {
    use crate::notebook_kernel::{Event, Request};
    let (_tmp, mut app, path, _kernel, started) = notebook_with_kernel("python3");
    // No kernel yet, and saved outputs and a count for a failed start to wipe.
    app.notebook_kernels.remove(&path);
    let saved = NOTEBOOK_355
        .replace("\"execution_count\": null", "\"execution_count\": 7")
        .replace(
            "\"outputs\": [],",
            "\"outputs\": [{\"output_type\": \"stream\", \"name\": \"stdout\", \"text\": [\"hello\\n\"]}],",
        );
    std::fs::write(&path, &saved).unwrap();
    app.editor.open(&path).unwrap();
    let before = app.editor.lines.join("\n");
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    assert_eq!(first_cell_label(&mut app, &mut term), "In [7]:");

    app.run_command(crate::widgets::command_palette::Command::NotebookRunAll);
    assert_eq!(app.status, "Starting kernel…");
    assert_eq!(first_cell_label(&mut app, &mut term), "In [*]:");
    started.borrow()[0]
        .1
        .send(Event::Error {
            message: "no Python with jupyter_client and ipykernel found".into(),
        })
        .unwrap();
    app.poll_notebook_kernels();
    assert_eq!(
        app.editor.lines.join("\n"),
        before,
        "outputs and count kept"
    );
    assert!(!app.editor.dirty, "nothing for Save to write");
    assert!(app.editor.notebook_running.is_empty());
    assert_eq!(first_cell_label(&mut app, &mut term), "In [7]:");
    assert_eq!(
        app.status,
        "Kernel: no Python with jupyter_client and ipykernel found"
    );
    assert!(!app.notebook_kernels.contains_key(&path));

    app.run_command(crate::widgets::command_palette::Command::NotebookRunAll);
    let started = started.borrow();
    assert_eq!(started.len(), 2, "the second run starts a new kernel");
    match started[1].2.try_iter().collect::<Vec<_>>().as_slice() {
        [Request::Execute { code, .. }] => assert_eq!(code, "print(1+1)"),
        other => panic!("the cell goes to the new kernel: {other:?}"),
    }
    assert_eq!(app.status, "Starting kernel…");
}

/// #878: Restart Kernel on a kernel whose bridge has gone starts a new one,
/// and the cells it owed stop waiting; on a live kernel it is only sent.
#[test]
fn restart_kernel_starts_a_new_kernel_once_the_bridge_is_gone() {
    use crate::notebook_kernel::{Event, NotebookRun, Request};
    let (_tmp, mut app, path, _kernel, started) = notebook_with_kernel("python3");
    let (run, kernel, requests) = NotebookRun::for_test();
    app.notebook_kernels.insert(path.clone(), run);
    kernel
        .send(Event::Ready {
            kernel: "python3".into(),
            display: "Python 3".into(),
        })
        .unwrap();
    app.poll_notebook_kernels();
    app.run_command(crate::widgets::command_palette::Command::NotebookRunAll);
    assert_eq!(app.editor.notebook_running, vec![0]);
    app.run_command(crate::widgets::command_palette::Command::NotebookRestart);
    assert!(
        requests.try_iter().any(|r| matches!(r, Request::Restart)),
        "a live kernel gets the restart"
    );
    assert!(started.borrow().is_empty());

    drop(requests);
    app.run_command(crate::widgets::command_palette::Command::NotebookRestart);
    assert_eq!(started.borrow().len(), 1, "a new kernel starts");
    assert!(app.editor.notebook_running.is_empty());
    assert_eq!(app.status, "Starting kernel…");
}

/// #1377: Notebook: Select Kernel lists the installed kernels, opened on
/// the notebook's own, and the choice is written into the notebook for its
/// next run.
#[test]
fn select_kernel_lists_the_installed_kernels_and_switches_the_notebook() {
    use crate::notebook_kernel::Event;
    let (_tmp, mut app, path, _kernel, started) = notebook_with_kernel("python3");
    app.notebook_kernels.remove(&path);
    app.run_command(crate::widgets::command_palette::Command::NotebookSelectKernel);
    {
        let started = started.borrow();
        let [(name, events, _)] = started.as_slice() else {
            panic!("one listing: {:?}", started.len());
        };
        assert_eq!(name, crate::notebook_kernel::LIST_KERNELS);
        events
            .send(Event::Specs {
                specs: installed_kernels(),
            })
            .unwrap();
    }
    assert!(app.list_picker.is_none(), "the picker waits for the list");
    app.poll_notebook_kernels();
    let picker = app.list_picker.as_ref().expect("the kernel picker opens");
    assert_eq!(picker.title, "Select Kernel");
    assert_eq!(
        picker.selected_row().map(|r| r.id.as_str()),
        Some("python3")
    );
    app.handle_key(key(KeyCode::Up, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let ks = notebook_kernelspec(&app);
    assert_eq!(ks["name"], "ir");
    assert_eq!(ks["language"], "R");
    assert_eq!(app.status, "Kernel: R (starts on the next run)");
    assert_eq!(started.borrow().len(), 1, "no kernel starts until a run");
    assert_eq!(
        crate::notebook_kernel::kernel_name(&app.editor.lines.join("\n")),
        "ir"
    );
}

/// Acceptance for #355 against a real kernel: `print(1+1)` shows `2`, and
/// after Save the file holds the output in the standard format. Set
/// `CROFT_TEST_JUPYTER_PYTHON` to a venv python with ipykernel.
#[test]
#[ignore = "needs a Python with ipykernel; set CROFT_TEST_JUPYTER_PYTHON"]
fn a_real_kernel_runs_a_cell_and_save_persists_the_output() {
    let python = std::path::PathBuf::from(std::env::var("CROFT_TEST_JUPYTER_PYTHON").unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let venv = python.parent().and_then(Path::parent).unwrap();
    std::os::unix::fs::symlink(venv, tmp.path().join(".venv")).unwrap();
    let path = tmp.path().join("t.ipynb");
    std::fs::write(&path, NOTEBOOK_355).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&path).unwrap();
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    click_first_cell_glyph(&mut app, &mut term);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !app
        .editor
        .lines
        .join("\n")
        .contains("\"execution_count\": 1")
    {
        assert!(std::time::Instant::now() < end, "status: {}", app.status);
        app.poll_notebook_kernels();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    app.save();
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["cells"][0]["outputs"][0]["output_type"], "stream");
    assert_eq!(saved["cells"][0]["outputs"][0]["text"][0], "2\n");
}

/// Acceptance for #355's figure criterion against a real kernel: a
/// matplotlib plot arrives as an `image/png` display, is saved in the file,
/// and the preview places it as an image block, the path Kitty and iTerm2
/// hosts draw. Needs matplotlib next to ipykernel.
#[test]
#[ignore = "needs a Python with ipykernel and matplotlib; set CROFT_TEST_JUPYTER_PYTHON"]
fn a_real_kernel_matplotlib_figure_becomes_an_inline_image() {
    let python = std::path::PathBuf::from(std::env::var("CROFT_TEST_JUPYTER_PYTHON").unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let venv = python.parent().and_then(Path::parent).unwrap();
    std::os::unix::fs::symlink(venv, tmp.path().join(".venv")).unwrap();
    let path = tmp.path().join("plot.ipynb");
    std::fs::write(
        &path,
        NOTEBOOK_355.replace(
            "\"print(1+1)\"",
            "\"import matplotlib.pyplot as plt\\n\", \"plt.plot([1, 3, 2])\\n\", \"plt.show()\"",
        ),
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&path).unwrap();
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
    click_first_cell_glyph(&mut app, &mut term);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !app
        .editor
        .lines
        .join("\n")
        .contains("\"execution_count\": 1")
    {
        assert!(std::time::Instant::now() < end, "status: {}", app.status);
        app.poll_notebook_kernels();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    term.draw(|f| app.render(f)).unwrap();
    let images = &app.editor.markdown_preview.as_ref().unwrap().images;
    assert_eq!(images.len(), 1, "the figure is one image block");
    let bytes = std::fs::read(&images[0].path).unwrap();
    assert!(bytes.starts_with(b"\x89PNG"), "a PNG in scratch");
    app.save();
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let outputs = saved["cells"][0]["outputs"].as_array().unwrap();
    assert!(
        outputs
            .iter()
            .any(|o| o["output_type"] == "display_data" && o["data"]["image/png"].is_string()),
        "{outputs:?}"
    );
}

/// #355: the notebook in the editor names its kernel in the status bar
/// once the kernel is up, and says when a cell is running.
#[test]
fn the_status_bar_names_the_notebooks_kernel_and_when_it_is_busy() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("t.ipynb");
    std::fs::write(&path, NOTEBOOK_355).unwrap();
    std::fs::write(tmp.path().join("other.py"), "x = 1\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&path).unwrap();
    let (run, kernel, _requests) = crate::notebook_kernel::NotebookRun::for_test();
    app.notebook_kernels.insert(path.clone(), run);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        !status_row(&term).contains("kernel:"),
        "no name before the kernel says it is up"
    );

    kernel
        .send(crate::notebook_kernel::Event::Ready {
            kernel: "python3".into(),
            display: "Python 3 (ipykernel)".into(),
        })
        .unwrap();
    app.poll_notebook_kernels();
    term.draw(|f| app.render(f)).unwrap();
    let row = status_row(&term);
    assert!(row.contains("kernel: Python 3 (ipykernel)"), "{row}");
    assert!(!row.contains("busy"), "{row}");

    click_first_cell_glyph(&mut app, &mut term);
    term.draw(|f| app.render(f)).unwrap();
    assert!(status_row(&term).contains("kernel: Python 3 (ipykernel) · busy"));

    // Another file in the editor has no kernel to name.
    app.editor.open(&tmp.path().join("other.py")).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert!(!status_row(&term).contains("kernel:"));
}
