//! `croft demo` (#377): a guided first tour, played in a throwaway sample
//! project so it never touches the user's own work.
//!
//! The script is data, `assets/tour/tour.json`: a list of steps, each an
//! action and the caption shown while it is on screen. Contributors extend
//! it, and a platform can ship its own (a Termux tour driven by the
//! on-screen keyboard, say) without touching this code.

#![cfg_attr(not(test), allow(dead_code))]

use std::path::{Path, PathBuf};

/// The built-in tour.
pub const TOUR_JSON: &str = include_str!("../assets/tour/tour.json");

/// The file that marks a folder as croft's own demo scratch space. Removal
/// refuses any folder without it, so a bug can never delete a workspace.
pub const SCRATCH_MARKER: &str = ".croft-demo";

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TourAction {
    /// Open a file, relative to the sample project.
    Open(String),
    QuickOpen,
    SplitEditor,
    Terminal,
    /// Type a command into the terminal and run it.
    Run(String),
    /// Open the first error the terminal shows.
    JumpToError,
    CommandPalette,
    ThemePicker,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct TourStep {
    pub action: TourAction,
    pub caption: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Tour {
    pub steps: Vec<TourStep>,
    /// The step on screen.
    #[serde(skip)]
    pub index: usize,
}

impl Tour {
    pub fn parse(json: &str) -> Result<Tour, String> {
        let tour: Tour = serde_json::from_str(json).map_err(|e| format!("tour.json: {e}"))?;
        if tour.steps.is_empty() {
            return Err(String::from("tour.json has no steps"));
        }
        Ok(tour)
    }

    pub fn current(&self) -> Option<&TourStep> {
        self.steps.get(self.index)
    }

    /// Move to the next step; `None` once the tour has run out.
    pub fn advance(&mut self) -> Option<&TourStep> {
        self.index = (self.index + 1).min(self.steps.len());
        self.current()
    }

    /// `3/9`, for the caption chip.
    pub fn progress(&self) -> String {
        format!(
            "{}/{}",
            (self.index + 1).min(self.steps.len()),
            self.steps.len()
        )
    }
}

static STARTUP_DEMO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `croft demo` asks for the tour here before the app starts.
pub fn request_startup_demo() {
    STARTUP_DEMO.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// The app takes the request once, after its first frame.
pub fn take_startup_demo() -> bool {
    STARTUP_DEMO.swap(false, std::sync::atomic::Ordering::SeqCst)
}

/// The sample project: a few languages, a build that fails on purpose so
/// the tour can show jumping to an error.
pub fn sample_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "README.md",
            "# croft tour\n\nA throwaway sample project. It is deleted when the tour ends.\n",
        ),
        (
            "src/main.rs",
            "fn main() {\n    let who = \"croft\";\n    println!(\"hello from {who}\");\n}\n",
        ),
        (
            "app.py",
            "def greet(name):\n    return f\"hello, {name}\"\n\nprint(greet(\"croft\")\n",
        ),
        (
            "web/index.html",
            "<!doctype html>\n<title>croft tour</title>\n<p>Hello from the sample project.</p>\n",
        ),
    ]
}

/// Create the sample project in a fresh folder under `parent`.
pub fn create_scratch(parent: &Path) -> std::io::Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = parent.join(format!("croft-demo-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join(SCRATCH_MARKER),
        "croft demo scratch; safe to delete\n",
    )?;
    for (rel, text) in sample_files() {
        let path = dir.join(rel);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, text)?;
    }
    Ok(dir)
}

/// Delete a scratch folder, but only one that carries [`SCRATCH_MARKER`].
pub fn remove_scratch(dir: &Path) -> Result<(), String> {
    if !dir.join(SCRATCH_MARKER).is_file() {
        return Err(format!(
            "{} is not a croft demo folder (no {SCRATCH_MARKER}); not deleting it",
            dir.display()
        ));
    }
    std::fs::remove_dir_all(dir).map_err(|e| e.to_string())
}

/// The first `file` and 1-based `line` an error names in terminal text:
/// Python's `File "app.py", line 3`, and the `path:line[:col]` form rustc,
/// gcc and most tools print.
pub fn first_error_ref(lines: &[String]) -> Option<(String, usize)> {
    for line in lines {
        // Python: File "app.py", line 3
        if let Some(rest) = line.split("File \"").nth(1)
            && let Some((file, tail)) = rest.split_once("\", line ")
            && let Some(n) = tail
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|d| d.parse().ok())
        {
            return Some((file.to_string(), n));
        }
        // path:line[:col], as rustc (after `-->`), gcc and most tools print.
        for token in line.split_whitespace() {
            let mut parts = token.split(':');
            let (Some(file), Some(n)) = (parts.next(), parts.next()) else {
                continue;
            };
            if file.contains('.')
                && !file.contains("//")
                && let Ok(n) = n.parse::<usize>()
            {
                return Some((file.to_string(), n));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_tour_parses_and_covers_the_issue_s_sequence() {
        let tour = Tour::parse(TOUR_JSON).unwrap();
        let actions: Vec<&TourAction> = tour.steps.iter().map(|s| &s.action).collect();
        assert_eq!(actions[0], &TourAction::Open("src/main.rs".into()));
        for want in [
            TourAction::QuickOpen,
            TourAction::SplitEditor,
            TourAction::Terminal,
            TourAction::JumpToError,
            TourAction::CommandPalette,
            TourAction::ThemePicker,
        ] {
            assert!(actions.contains(&&want), "{want:?} in the tour");
        }
        assert_eq!(actions.last(), Some(&&TourAction::Done));
        assert!(tour.steps.iter().all(|s| !s.caption.is_empty()));
        // Every file the tour opens exists in the sample project.
        let files: Vec<&str> = sample_files().iter().map(|(p, _)| *p).collect();
        for s in &tour.steps {
            if let TourAction::Open(p) = &s.action {
                assert!(files.contains(&p.as_str()), "{p} is in the sample");
            }
        }
    }

    #[test]
    fn the_startup_demo_request_is_taken_once() {
        request_startup_demo();
        assert!(take_startup_demo());
        assert!(!take_startup_demo());
    }

    #[test]
    fn a_tour_steps_forward_and_ends() {
        let mut t = Tour::parse(
            r#"{"steps":[{"action":"quick_open","caption":"a"},{"action":"done","caption":"b"}]}"#,
        )
        .unwrap();
        assert_eq!(t.current().unwrap().caption, "a");
        assert_eq!(t.progress(), "1/2");
        assert_eq!(t.advance().unwrap().caption, "b");
        assert_eq!(t.progress(), "2/2");
        assert!(t.advance().is_none());
        assert!(t.current().is_none(), "past the end");
    }

    #[test]
    fn a_malformed_tour_is_an_error() {
        assert!(Tour::parse("{}").is_err());
        assert!(Tour::parse(r#"{"steps":[{"action":"fly","caption":"x"}]}"#).is_err());
        assert!(
            Tour::parse(r#"{"steps":[]}"#).is_err(),
            "an empty tour is not a tour"
        );
    }

    #[test]
    fn scratch_is_created_marked_and_only_marked_folders_are_removed() {
        let parent = tempfile::tempdir().unwrap();
        let dir = create_scratch(parent.path()).unwrap();
        assert!(dir.join(SCRATCH_MARKER).is_file());
        for (p, _) in sample_files() {
            assert!(dir.join(p).is_file(), "{p} written");
        }
        remove_scratch(&dir).unwrap();
        assert!(!dir.exists());

        let real = tempfile::tempdir().unwrap();
        std::fs::write(real.path().join("important.txt"), "keep").unwrap();
        assert!(remove_scratch(real.path()).is_err(), "unmarked: refused");
        assert!(
            real.path().join("important.txt").exists(),
            "nothing deleted"
        );
    }

    #[test]
    fn the_sample_build_fails_and_the_tour_can_find_the_line() {
        // The tour's run step is `python3 -m py_compile app.py`; it has to
        // fail, and its output has to lead `first_error_ref` to app.py.
        let parent = tempfile::tempdir().unwrap();
        let dir = create_scratch(parent.path()).unwrap();
        let out = std::process::Command::new("python3")
            .args(["-m", "py_compile", "app.py"])
            .current_dir(&dir)
            .output()
            .expect("python3 is available");
        assert!(!out.status.success(), "the sample build fails on purpose");
        let text = String::from_utf8_lossy(&out.stderr);
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        let (file, line) = first_error_ref(&lines).expect("an error location");
        assert!(file.ends_with("app.py"), "{file}");
        assert!(line >= 1);
    }

    #[test]
    fn errors_are_found_in_python_and_path_line_forms() {
        let py = vec![
            "$ python3 -m py_compile app.py".to_string(),
            "  File \"app.py\", line 3".to_string(),
            "    print(\"hello\"".to_string(),
        ];
        assert_eq!(first_error_ref(&py), Some(("app.py".into(), 3)));
        let rust = vec![
            "error[E0425]: nope".to_string(),
            "  --> src/main.rs:12:5".to_string(),
        ];
        assert_eq!(first_error_ref(&rust), Some(("src/main.rs".into(), 12)));
        let gcc = vec!["main.c:7:3: error: expected ';'".to_string()];
        assert_eq!(first_error_ref(&gcc), Some(("main.c".into(), 7)));
        assert_eq!(first_error_ref(&["all good".to_string()]), None);
    }
}
