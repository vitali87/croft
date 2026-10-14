//! Lines of text held in one allocation (#1707).
//!
//! A `Vec<String>` gives every line a 24-byte header and an allocation of
//! its own, which the allocator rounds up and tags: about 45 bytes a line
//! on top of the text, more than the text itself for most source code. The
//! copies croft keeps beside a buffer (the text its syntax colors were
//! computed for, the HEAD version the git gutter diffs against, each undo
//! step) are read and compared but never edited a character at a time, so
//! they live here instead: the lines joined by `\n`, and where each one
//! ends.

/// Lines joined in one `String`, each found by where it ends.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackedLines {
    /// The lines, each followed by `\n` except the last.
    text: String,
    /// Where each line ends in `text`, exclusive of its `\n`.
    ends: Vec<usize>,
}

impl PackedLines {
    /// `text` split at each `\n`, as `str::split` splits it: a trailing
    /// newline leaves an empty last line, and an empty text is one empty
    /// line. Takes the text over without copying it.
    pub fn from_joined(text: String) -> Self {
        let mut ends: Vec<usize> = memchr::memchr_iter(b'\n', text.as_bytes()).collect();
        ends.push(text.len());
        ends.shrink_to_fit();
        Self { text, ends }
    }

    /// Add `line` after the last.
    pub fn push(&mut self, line: &str) {
        if !self.ends.is_empty() {
            self.text.push('\n');
        }
        self.text.push_str(line);
        self.ends.push(self.text.len());
    }

    /// How many lines there are.
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    /// The heap this holds, in bytes.
    pub fn heap_bytes(&self) -> usize {
        self.text.capacity() + self.ends.capacity() * std::mem::size_of::<usize>()
    }

    /// Line `i`, or `None` past the last.
    pub fn get(&self, i: usize) -> Option<&str> {
        let end = *self.ends.get(i)?;
        Some(&self.text[self.start(i)..end])
    }

    /// The lines in order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &str> + ExactSizeIterator {
        self.lines_in(0..self.len())
    }

    /// Lines `range`, in order.
    pub fn lines_in(
        &self,
        range: std::ops::Range<usize>,
    ) -> impl DoubleEndedIterator<Item = &str> + ExactSizeIterator {
        range.map(|i| &self.text[self.start(i)..self.ends[i]])
    }

    /// Replace lines `range` with `with`, as `Vec::splice` would.
    pub fn splice<S: AsRef<str>>(&mut self, range: std::ops::Range<usize>, with: &[S]) {
        let (from, to, n) = (range.start, range.end, self.len());
        let joined = with
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<_>>()
            .join("\n");
        // The bytes replaced, what replaces them, and where the first new
        // line starts.
        let (a, b, insert, first) = match (with.is_empty(), from == to) {
            (true, true) => return,
            // Removed lines take the `\n` after them with them, or the last
            // lines the one before them.
            (true, false) if to < n => (self.start(from), self.start(to), String::new(), 0),
            (true, false) if from > 0 => (self.ends[from - 1], self.text.len(), String::new(), 0),
            (true, false) => (0, self.text.len(), String::new(), 0),
            // Replaced lines leave the `\n`s around them where they are.
            (false, false) => {
                let at = self.start(from);
                (at, self.ends[to - 1], joined, at)
            }
            // Inserted lines go in before line `from`, or after the last.
            (false, true) if from < n => {
                let at = self.start(from);
                (at, at, joined + "\n", at)
            }
            (false, true) if n > 0 => {
                let at = self.text.len();
                (at, at, format!("\n{joined}"), at + 1)
            }
            (false, true) => (0, 0, joined, 0),
        };
        let mut inserted = Vec::with_capacity(with.len());
        let mut at = first;
        for (k, line) in with.iter().enumerate() {
            at += usize::from(k > 0) + line.as_ref().len();
            inserted.push(at);
        }
        let shift = insert.len() as isize - (b - a) as isize;
        self.text.replace_range(a..b, &insert);
        let tail = self.ends.split_off(to);
        self.ends.truncate(from);
        self.ends.extend(inserted);
        self.ends
            .extend(tail.into_iter().map(|end| end.wrapping_add_signed(shift)));
    }

    /// Where line `i` starts in `text`.
    fn start(&self, i: usize) -> usize {
        match i {
            0 => 0,
            _ => self.ends[i - 1] + 1,
        }
    }
}

impl<S: AsRef<str>> From<&[S]> for PackedLines {
    fn from(lines: &[S]) -> Self {
        let mut text = String::with_capacity(lines.iter().map(|l| l.as_ref().len() + 1).sum());
        let mut ends = Vec::with_capacity(lines.len());
        for (k, line) in lines.iter().enumerate() {
            if k > 0 {
                text.push('\n');
            }
            text.push_str(line.as_ref());
            ends.push(text.len());
        }
        Self { text, ends }
    }
}

impl<'a> FromIterator<&'a str> for PackedLines {
    fn from_iter<I: IntoIterator<Item = &'a str>>(lines: I) -> Self {
        let mut packed = Self::default();
        for line in lines {
            packed.push(line);
        }
        packed.text.shrink_to_fit();
        packed.ends.shrink_to_fit();
        packed
    }
}

impl From<Vec<String>> for PackedLines {
    fn from(lines: Vec<String>) -> Self {
        Self::from(&lines[..])
    }
}

impl<S: AsRef<str>> PartialEq<[S]> for PackedLines {
    fn eq(&self, other: &[S]) -> bool {
        self.len() == other.len() && self.iter().zip(other).all(|(a, b)| a == b.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(p: &PackedLines) -> Vec<&str> {
        p.iter().collect()
    }

    #[test]
    fn splits_like_str_split() {
        for text in ["", "a", "a\n", "a\nb", "\n\n", "a\n\nb\n"] {
            let p = PackedLines::from_joined(text.to_string());
            assert_eq!(lines(&p), text.split('\n').collect::<Vec<_>>(), "{text:?}");
            assert_eq!(p.len(), text.split('\n').count());
        }
    }

    #[test]
    fn built_from_lines_reads_them_back() {
        let v = ["one".to_string(), String::new(), "three".to_string()];
        let p = PackedLines::from(&v[..]);
        assert_eq!(lines(&p), ["one", "", "three"]);
        assert_eq!(p.get(2), Some("three"));
        assert_eq!(p.get(3), None);
        assert_eq!(p.lines_in(1..3).collect::<Vec<_>>(), ["", "three"]);
        let vec_bytes: usize = v.iter().map(|l| l.capacity() + 24).sum();
        assert!(
            p.heap_bytes() < vec_bytes,
            "{} >= {vec_bytes}",
            p.heap_bytes()
        );
        assert!(p == v[..]);
        assert_eq!(p, PackedLines::from_joined("one\n\nthree".into()));
        assert_eq!(PackedLines::from(&[] as &[&str]).len(), 0);
        let collected: PackedLines = "one\n\nthree\n".lines().collect();
        assert_eq!(collected, p, "collected from an iterator of lines");
    }

    #[test]
    fn compares_line_by_line() {
        let p = PackedLines::from(&["a", "b"][..]);
        assert!(p == ["a", "b"][..]);
        assert!(p != ["a"][..]);
        assert!(p != ["a", "c"][..]);
        assert!(p != ["a\nb"][..], "one line holding a newline is not two");
    }

    /// Every splice a `Vec<String>` can take leaves the packed lines equal to
    /// the vector after the same splice.
    #[test]
    fn splices_as_a_vec_does() {
        let bases: [&[&str]; 4] = [&[], &[""], &["a"], &["a", "bb", "", "ccc"]];
        let inserts: [&[&str]; 4] = [&[], &["x"], &["x", ""], &["", "yy", "z"]];
        for base in bases {
            for from in 0..=base.len() {
                for to in from..=base.len() {
                    for with in inserts {
                        let mut want: Vec<&str> = base.to_vec();
                        want.splice(from..to, with.iter().copied());
                        let mut p = PackedLines::from(base);
                        p.splice(from..to, with);
                        let case = format!("{base:?}[{from}..{to}] <- {with:?}");
                        assert_eq!(lines(&p), want, "{case}");
                        assert_eq!(p, PackedLines::from(&want[..]), "{case}");
                    }
                }
            }
        }
    }
}
