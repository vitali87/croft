//! The query field the pickers share: the text typed into Quick Open, the
//! Command Palette, both symbol pickers and the directory, branch and list
//! pickers, with its caret counted in characters, not bytes. Each picker
//! keeps its own `query` and `cursor` and edits them through these. What an
//! edit does to the results (ranking them again, a fresh selection) stays
//! with the picker, so one keystroke never ranks them twice.

/// The byte offset of character `char_idx` in `query`, or its length when
/// `char_idx` is at or past the end.
pub fn byte_offset(query: &str, char_idx: usize) -> usize {
    query
        .char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(query.len())
}

/// How many characters `query` holds: the caret's last place.
pub fn char_count(query: &str) -> usize {
    query.chars().count()
}

/// Insert `c` at the caret and move the caret past it.
pub fn insert(query: &mut String, caret: &mut usize, c: char) {
    query.insert(byte_offset(query, *caret), c);
    *caret += 1;
}

/// Remove the character before the caret and move the caret back onto its
/// place. At the start nothing changes and the answer is false.
pub fn backspace(query: &mut String, caret: &mut usize) -> bool {
    if *caret == 0 {
        return false;
    }
    query.remove(byte_offset(query, *caret - 1));
    *caret -= 1;
    true
}

/// Remove the character after the caret. At the end nothing changes and the
/// answer is false.
pub fn delete(query: &mut String, caret: usize) -> bool {
    if caret >= char_count(query) {
        return false;
    }
    query.remove(byte_offset(query, caret));
    true
}

/// Move the caret one character left, stopping at the start.
pub fn left(caret: &mut usize) {
    *caret = caret.saturating_sub(1);
}

/// Move the caret one character right, stopping at the end.
pub fn right(query: &str, caret: &mut usize) {
    *caret = (*caret + 1).min(char_count(query));
}

/// Move the caret to the end.
pub fn end(query: &str, caret: &mut usize) {
    *caret = char_count(query);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_puts_the_character_at_the_caret_and_moves_past_it() {
        let (mut q, mut c) = (String::from("ad"), 1);
        insert(&mut q, &mut c, 'b');
        insert(&mut q, &mut c, 'c');
        assert_eq!((q.as_str(), c), ("abcd", 3));
        let (mut q, mut c) = (String::new(), 0);
        insert(&mut q, &mut c, 'x');
        assert_eq!((q.as_str(), c), ("x", 1));
    }

    #[test]
    fn the_caret_counts_characters_not_bytes() {
        // "é" and "日" take two and three bytes; the caret steps over each.
        let (mut q, mut c) = (String::from("é日"), 2);
        insert(&mut q, &mut c, 'z');
        assert_eq!((q.as_str(), c), ("é日z", 3));
        assert!(backspace(&mut q, &mut c));
        assert!(backspace(&mut q, &mut c));
        assert_eq!((q.as_str(), c), ("é", 1));
        let (mut q, c) = (String::from("é日"), 0);
        assert!(delete(&mut q, c));
        assert_eq!(q, "日");
        assert_eq!(byte_offset("é日", 1), 2);
        assert_eq!(byte_offset("é日", 2), 5);
        assert_eq!(byte_offset("é日", 9), 5);
    }

    #[test]
    fn backspace_at_the_start_and_delete_at_the_end_change_nothing() {
        let (mut q, mut c) = (String::from("ab"), 0);
        assert!(!backspace(&mut q, &mut c));
        assert_eq!((q.as_str(), c), ("ab", 0));
        assert!(!delete(&mut q, 2));
        assert_eq!(q, "ab");
        let (mut q, mut c) = (String::new(), 0);
        assert!(!backspace(&mut q, &mut c));
        assert!(!delete(&mut q, c));
    }

    #[test]
    fn backspace_and_delete_remove_one_character_either_side_of_the_caret() {
        let (mut q, mut c) = (String::from("abcd"), 2);
        assert!(backspace(&mut q, &mut c));
        assert_eq!((q.as_str(), c), ("acd", 1));
        assert!(delete(&mut q, c));
        assert_eq!((q.as_str(), c), ("ad", 1));
    }

    #[test]
    fn the_caret_moves_within_the_query_and_stops_at_both_ends() {
        let q = "abc";
        let mut c = 1;
        left(&mut c);
        left(&mut c);
        assert_eq!(c, 0);
        right(q, &mut c);
        right(q, &mut c);
        right(q, &mut c);
        right(q, &mut c);
        assert_eq!(c, 3);
        c = 0;
        end(q, &mut c);
        assert_eq!(c, 3);
        let mut c = 1;
        end("日本", &mut c);
        assert_eq!(c, 2);
    }
}
