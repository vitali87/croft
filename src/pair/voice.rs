//! Where the pilot's words go: the model's prose and the pilot's diagnostics, to the
//! in-process host when seated inside croft, else the REPL's stdout and stderr.

use super::*;

/// Host and child DIAGNOSTICS, never the model's voice. Seated in croft
/// they go to the OUTPUT channel: [`say`]'s Commentary events are reserved
/// for the model's own prose, which the App anchors as a navigator comment
/// box — a diagnostic riding that channel was rendered as the navigator's
/// remark in the user's file and counted as one of its comments. The REPL
/// driver prints to stdout, its only voice.
pub(super) fn diag(state: &Mutex<PairState>, level: crate::output::OutputLevel, text: &str) {
    // Bounded (see lock_briefly): diagnostics also fire from paths a
    // wedged pilot would block. Unknown seating falls back to OUTPUT — the
    // right sink when seated, and for the REPL a diagnostic in OUTPUT is
    // the lesser loss (stdout is unreachable without the lock, and stderr
    // would corrupt a seated alternate screen).
    let seated = lock_briefly(state).is_none_or(|st| st.events.is_some());
    if seated {
        crate::output::push("Navigator", level, text);
    } else {
        println!("{text}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
}

/// The pilot's voice: an event to the in-process host when seated inside
/// croft (stdout belongs to the TUI there), else the debug REPL's stdout.
pub(super) fn say(state: &Mutex<PairState>, text: &str) {
    let sink = state.lock().unwrap().events.clone();
    match sink {
        Some(tx) => {
            let _ = tx.send(crate::pair_host::PairEvent::Commentary(text.to_string()));
        }
        None => {
            print!("{text}");
            let _ = std::io::Stdout::flush(&mut std::io::stdout());
        }
    }
}
