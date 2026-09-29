//! SARIF's markdown messages (§3.11.4, GitHub Flavored Markdown) laid out
//! as plain lines for the details pane (#577).
//!
//! The pane styles whole lines, so emphasis and inline code keep their
//! words and lose their markers; headings and code blocks are marked by
//! their line kind. Links read the way the plain-text message shows them:
//! a location link (`[text](3)`) as `[text]`, a URI link as `text <uri>`.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Text,
    Heading,
    Code,
}

/// `md` as lines of at most `width` columns. Paragraphs are separated by a
/// blank line; code block lines are indented and never wrapped.
pub fn lines(md: &str, width: usize) -> Vec<(String, LineKind)> {
    let mut b = Builder {
        out: Vec::new(),
        cur: String::new(),
        prefix: String::new(),
        width: width.max(1),
    };
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut links: Vec<String> = Vec::new();
    let mut heading = false;
    let mut code = false;
    let mut quote = 0usize;
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(md, options) {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                b.flush(LineKind::Text);
                heading = true;
            }
            Event::End(TagEnd::Heading(_)) => {
                b.flush(LineKind::Heading);
                heading = false;
                b.blank();
            }
            Event::End(TagEnd::Paragraph) => {
                b.flush(LineKind::Text);
                if lists.is_empty() {
                    b.blank();
                }
            }
            Event::Start(Tag::List(start)) => {
                b.flush(LineKind::Text);
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                b.flush(LineKind::Text);
                lists.pop();
                if lists.is_empty() {
                    b.blank();
                }
            }
            Event::Start(Tag::Item) => {
                b.flush(LineKind::Text);
                let depth = lists.len().saturating_sub(1);
                let bullet = match lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{}. ", *n - 1)
                    }
                    _ => String::from("• "),
                };
                b.prefix = format!("{}{}{bullet}", "│ ".repeat(quote), "  ".repeat(depth));
            }
            Event::End(TagEnd::Item) => {
                b.flush(LineKind::Text);
                b.prefix = "│ ".repeat(quote);
            }
            Event::Start(Tag::BlockQuote(_)) => {
                b.flush(LineKind::Text);
                quote += 1;
                b.prefix = "│ ".repeat(quote);
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                b.flush(LineKind::Text);
                quote -= 1;
                b.prefix = "│ ".repeat(quote);
            }
            Event::Start(Tag::CodeBlock(_)) => {
                b.flush(LineKind::Text);
                code = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                code = false;
                b.blank();
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                if is_location_link(&dest_url) {
                    b.cur.push('[');
                }
                links.push(dest_url.to_string());
            }
            Event::End(TagEnd::Link) => {
                let dest = links.pop().unwrap_or_default();
                if is_location_link(&dest) {
                    b.cur.push(']');
                } else if !dest.is_empty() && !b.cur.ends_with(dest.as_str()) {
                    b.cur.push_str(&format!(" <{dest}>"));
                }
            }
            Event::Text(t) if code => {
                for l in t.lines() {
                    b.out.push((format!("{}    {l}", b.prefix), LineKind::Code));
                }
            }
            Event::Text(t) | Event::Code(t) | Event::InlineHtml(t) | Event::Html(t) => {
                b.cur.push_str(&t)
            }
            Event::SoftBreak => b.cur.push(' '),
            Event::HardBreak => b.flush(if heading {
                LineKind::Heading
            } else {
                LineKind::Text
            }),
            Event::Rule => {
                b.flush(LineKind::Text);
                b.out.push(("─".repeat(b.width.min(40)), LineKind::Text));
                b.blank();
            }
            Event::TaskListMarker(done) => b.cur.push_str(if done { "[x] " } else { "[ ] " }),
            _ => {}
        }
    }
    b.flush(LineKind::Text);
    while b.out.last().is_some_and(|(l, _)| l.is_empty()) {
        b.out.pop();
    }
    b.out
}

/// A `[text](n)` link: its target is a location id (§3.11.6).
fn is_location_link(dest: &str) -> bool {
    !dest.is_empty() && dest.chars().all(|c| c.is_ascii_digit())
}

struct Builder {
    out: Vec<(String, LineKind)>,
    cur: String,
    /// Written before the first line of the pending text (a bullet), then
    /// as that many spaces before the lines it wraps to.
    prefix: String,
    width: usize,
}

impl Builder {
    fn flush(&mut self, kind: LineKind) {
        if self.cur.trim().is_empty() {
            self.cur.clear();
            return;
        }
        let text = std::mem::take(&mut self.cur);
        let lead = self.prefix.chars().count();
        let hang: String = self
            .prefix
            .chars()
            .map(|c| if c == '│' { c } else { ' ' })
            .collect();
        let room = self.width.saturating_sub(lead).max(1);
        for (i, l) in super::render::wrap(text.trim(), room)
            .into_iter()
            .enumerate()
        {
            let p = if i == 0 { &self.prefix } else { &hang };
            self.out.push((format!("{p}{l}"), kind));
        }
        // A bullet belongs to its item's first line only.
        self.prefix = hang;
    }

    fn blank(&mut self) {
        if self.out.last().is_some_and(|(l, _)| !l.is_empty()) {
            self.out.push((String::new(), LineKind::Text));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(md: &str, width: usize) -> Vec<String> {
        lines(md, width).into_iter().map(|(l, _)| l).collect()
    }

    #[test]
    fn emphasis_and_code_keep_their_words_and_lose_their_markers() {
        assert_eq!(
            text("Building a query from **user input** with `format!`.", 80),
            ["Building a query from user input with format!."]
        );
    }

    #[test]
    fn links_read_as_the_plain_message_shows_them() {
        assert_eq!(
            text(
                "Data from [the source](1) reaches [docs](https://x.test/a).",
                80
            ),
            ["Data from [the source] reaches docs <https://x.test/a>."]
        );
        // An autolink's text is its URL: not repeated.
        assert_eq!(text("<https://x.test/a>", 80), ["https://x.test/a"]);
    }

    #[test]
    fn headings_lists_and_code_blocks_get_their_own_lines() {
        let md = "# Fix\n\nUse:\n\n- parameters\n- an ORM\n  1. first\n\n```\nq(?)\n```\n";
        let out = lines(md, 80);
        assert_eq!(out[0], ("Fix".to_string(), LineKind::Heading));
        let all: Vec<&str> = out.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            all,
            [
                "Fix",
                "",
                "Use:",
                "",
                "• parameters",
                "• an ORM",
                "  1. first",
                "",
                "    q(?)"
            ]
        );
        assert_eq!(out.last().unwrap().1, LineKind::Code);
    }

    #[test]
    fn a_wrapped_item_hangs_under_its_text() {
        assert_eq!(
            text("- one two three four", 10),
            ["• one two", "  three", "  four"]
        );
    }
}
