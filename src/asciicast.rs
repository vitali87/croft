//! Writing a session as an asciicast v2 recording (#356).
//!
//! Every demo of croft is currently made with a third-party recorder, while
//! the session host already produces the exact byte stream one needs, with
//! timestamps. This is the serialisation half: turning that stream into the
//! format `asciinema play` and the `agg` GIF converter already read.
//!
//! # The format, and the two things that break it
//!
//! A cast is JSON Lines: a header object, then one array per event —
//! `[time, "o", data]` for output and `[time, "r", "COLSxROWS"]` for a
//! resize. Both traps are in that description.
//!
//! **The data is a JSON string, not bytes.** A terminal stream is full of
//! control characters, and a raw `"` or `\` or `\u{1b}` written literally
//! produces a file that is not JSON at all — the player fails on the whole
//! recording, not on the one frame. Every payload goes through a real JSON
//! encoder for that reason rather than through `format!`.
//!
//! **Time is relative to the start and must not go backwards.** A player
//! reading a decreasing timestamp either sleeps forever or panics, depending
//! on the player. `SystemTime` can step backwards across an NTP correction,
//! so the clock is clamped rather than trusted: a frame that would go
//! backwards is written at the previous frame's time, which shows as a burst
//! rather than as a broken file.

/// One recorded event.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Output written to the terminal.
    Output { at: f64, data: String },
    /// The terminal was resized.
    Resize { at: f64, cols: u16, rows: u16 },
}

/// An asciicast v2 writer.
#[derive(Debug)]
pub struct Recorder {
    cols: u16,
    rows: u16,
    /// The last timestamp written, so the stream cannot go backwards.
    last: f64,
}

impl Recorder {
    /// Start a recording of a `cols` x `rows` terminal.
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            last: 0.0,
        }
    }

    /// The header line, which must be the file's first line.
    ///
    /// Built through `serde_json` like the events: a title carrying a quote
    /// would otherwise produce a header that parses as nothing, and the
    /// player reports "not an asciicast" rather than naming the field.
    pub fn header(&self, title: Option<&str>) -> String {
        let mut obj = serde_json::Map::new();
        obj.insert("version".into(), serde_json::json!(2));
        obj.insert("width".into(), serde_json::json!(self.cols));
        obj.insert("height".into(), serde_json::json!(self.rows));
        if let Some(t) = title {
            obj.insert("title".into(), serde_json::json!(t));
        }
        serde_json::Value::Object(obj).to_string()
    }

    /// Serialise one event, clamping its time so the stream never goes
    /// backwards.
    ///
    /// Takes `&mut self` because the clamp is state: the alternative is a
    /// pure function the caller has to remember to feed the previous time,
    /// and a caller that forgets writes a file no player can read.
    pub fn line(&mut self, event: &Event) -> String {
        let at = match event {
            Event::Output { at, .. } | Event::Resize { at, .. } => *at,
        };
        // Clamp rather than reject. A backwards step is almost always a
        // clock correction of a few milliseconds, and collapsing it into the
        // previous instant shows as a burst — where dropping the frame would
        // lose output the user saw, and writing it raw would break playback
        // for everything after it.
        let at = if at < self.last { self.last } else { at };
        self.last = at;

        let value = match event {
            Event::Output { data, .. } => serde_json::json!([at, "o", data]),
            Event::Resize { cols, rows, .. } => {
                serde_json::json!([at, "r", format!("{cols}x{rows}")])
            }
        };
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line is valid JSON, including one carrying the control
    /// characters a terminal stream is made of.
    ///
    /// This is the trap the format invites: `format!("[{at}, \"o\", \"{data}\"]")`
    /// reads naturally and produces a file that is not JSON the moment the
    /// stream contains a quote, a backslash or an escape byte — which is
    /// every real session. The player then rejects the WHOLE recording, not
    /// the one frame.
    #[test]
    fn a_control_heavy_payload_still_parses_as_json() {
        let mut rec = Recorder::new(80, 24);
        // A real prompt: an OSC title, a colour SGR, a quote, a backslash,
        // a tab, a CR, and a newline.
        let nasty = "\u{1b}]0;t\u{7}\u{1b}[32m\"q\\p\ttail\r\n";
        let line = rec.line(&Event::Output {
            at: 0.5,
            data: String::from(nasty),
        });

        let parsed: serde_json::Value =
            serde_json::from_str(&line).expect("every event line must be valid JSON");
        assert_eq!(parsed[1], "o");
        assert_eq!(
            parsed[2].as_str(),
            Some(nasty),
            "the payload must round-trip byte for byte"
        );
        // And the raw line carries no literal control byte that would break
        // the JSON Lines framing.
        assert!(
            !line.contains('\n') && !line.contains('\r'),
            "a raw newline in the line splits one event into two: {line:?}"
        );
    }

    /// The header is the first line and describes the terminal.
    #[test]
    fn the_header_names_the_version_and_size() {
        let rec = Recorder::new(120, 40);
        let parsed: serde_json::Value =
            serde_json::from_str(&rec.header(None)).expect("the header must be JSON");
        assert_eq!(parsed["version"], 2);
        assert_eq!(parsed["width"], 120);
        assert_eq!(parsed["height"], 40);
        assert!(parsed.get("title").is_none(), "no title unless given one");

        // A title containing a quote must not break the header — that would
        // make the player reject the file as "not an asciicast" rather than
        // name the field.
        let titled = rec.header(Some("say \"hi\"\\now"));
        let parsed: serde_json::Value =
            serde_json::from_str(&titled).expect("a quoted title must still parse");
        assert_eq!(parsed["title"], "say \"hi\"\\now");
    }

    /// Time never goes backwards, because a player reading a decreasing
    /// timestamp either sleeps forever or panics.
    ///
    /// `SystemTime` can step back across an NTP correction, so the clock is
    /// clamped rather than trusted. Clamping shows as a burst; the
    /// alternatives lose output the user saw, or break playback for
    /// everything after the bad frame.
    #[test]
    fn a_backwards_clock_is_clamped_rather_than_written() {
        let mut rec = Recorder::new(80, 24);
        let at = |line: &str| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()[0]
                .as_f64()
                .unwrap()
        };

        let a = rec.line(&Event::Output {
            at: 1.0,
            data: String::from("one"),
        });
        // The clock steps back half a second.
        let b = rec.line(&Event::Output {
            at: 0.5,
            data: String::from("two"),
        });
        let c = rec.line(&Event::Output {
            at: 2.0,
            data: String::from("three"),
        });

        assert_eq!(at(&a), 1.0);
        assert_eq!(at(&b), 1.0, "a backwards frame collapses onto the previous");
        assert_eq!(at(&c), 2.0, "and the stream continues from there");
        // The frame is KEPT, not dropped — the user saw that output.
        let parsed: serde_json::Value = serde_json::from_str(&b).unwrap();
        assert_eq!(parsed[2], "two");
    }

    /// A resize is an `r` event carrying `COLSxROWS`.
    #[test]
    fn a_resize_is_written_as_an_r_event() {
        let mut rec = Recorder::new(80, 24);
        let line = rec.line(&Event::Resize {
            at: 3.25,
            cols: 132,
            rows: 50,
        });
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed[0], 3.25);
        assert_eq!(parsed[1], "r");
        assert_eq!(
            parsed[2], "132x50",
            "asciinema reads COLSxROWS; the other order silently transposes \
             the playback"
        );
        // A resize is clamped by the same clock as output, so the two kinds
        // cannot interleave out of order.
        let back = rec.line(&Event::Output {
            at: 1.0,
            data: String::from("late"),
        });
        let parsed: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(parsed[0], 3.25);
    }
}
