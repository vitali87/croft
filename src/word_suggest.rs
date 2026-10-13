//! Word-based completion (#1566), VS Code's `editor.wordBasedSuggestions`:
//! the words already in the open documents, offered when no language server
//! answers. A Makefile, a `.env` or a plain-text note has no server, and
//! without this Ctrl+Space there offered nothing at all, not even a name
//! typed a line above.

/// Which documents the words come from (`word_based_suggestions`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WordSuggestions {
    /// No word suggestions.
    Off,
    /// The words of the document being edited.
    CurrentDocument,
    /// Those, and the words of the other open documents in its language.
    /// VS Code's default, and croft's.
    #[default]
    MatchingDocuments,
    /// Those, and the words of every other open document.
    AllDocuments,
}

impl WordSuggestions {
    /// The config ids, VS Code's own. Anything else, the empty string an
    /// older config reads as included, is the default.
    pub fn from_pref(s: &str) -> Self {
        match s {
            "off" => Self::Off,
            "currentDocument" => Self::CurrentDocument,
            "allDocuments" => Self::AllDocuments,
            _ => Self::MatchingDocuments,
        }
    }
}

/// The most words one popup carries. A large file holds tens of thousands;
/// the nearest are the ones worth offering.
pub const MAX_WORDS: usize = 2000;

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Each word of `line` with its char columns `[start, end)`. A word is a run
/// of letters, digits and `_`, the editor's own word, so it is the same span
/// the completion prefix and its replacement cover.
fn words_of(line: &str) -> impl Iterator<Item = (usize, usize, String)> + '_ {
    let mut chars = line.chars().enumerate().peekable();
    std::iter::from_fn(move || {
        while chars.peek().is_some_and(|&(_, c)| !is_word_char(c)) {
            chars.next();
        }
        let &(start, _) = chars.peek()?;
        let mut word = String::new();
        let mut end = start;
        while let Some(&(i, c)) = chars.peek() {
            if !is_word_char(c) {
                break;
            }
            word.push(c);
            end = i + 1;
            chars.next();
        }
        Some((start, end, word))
    })
}

/// The words starting with `prefix` (ignoring case), nearest the caret
/// first: those of `current` by their distance in lines from the caret's
/// row, then those of `others` in the order given. Left out are the word
/// the caret is in, which is what is being typed, a word equal to the
/// prefix, which would complete to itself, and a run of digits alone,
/// which is a number rather than a name.
pub fn collect(
    current: &[String],
    caret: (usize, usize),
    others: &[&[String]],
    prefix: &str,
) -> Vec<String> {
    let needle = prefix.to_lowercase();
    let wanted = |w: &str| {
        w != prefix
            && !w.chars().all(|c| c.is_ascii_digit())
            && w.to_lowercase().starts_with(&needle)
    };
    let mut nearest: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let (crow, ccol) = caret;
    for (row, line) in current.iter().enumerate() {
        for (start, end, word) in words_of(line) {
            if row == crow && start <= ccol && ccol <= end {
                continue;
            }
            if !wanted(&word) {
                continue;
            }
            let dist = row.abs_diff(crow);
            nearest
                .entry(word)
                .and_modify(|d| *d = (*d).min(dist))
                .or_insert(dist);
        }
    }
    let mut ranked: Vec<(usize, String)> = nearest.into_iter().map(|(w, d)| (d, w)).collect();
    ranked.sort();
    let mut seen: std::collections::HashSet<String> =
        ranked.iter().map(|(_, w)| w.clone()).collect();
    let mut out: Vec<String> = ranked.into_iter().map(|(_, w)| w).collect();
    out.truncate(MAX_WORDS);
    for doc in others {
        for line in doc.iter() {
            for (_, _, word) in words_of(line) {
                if out.len() >= MAX_WORDS {
                    return out;
                }
                if wanted(&word) && seen.insert(word.clone()) {
                    out.push(word);
                }
            }
        }
    }
    out.truncate(MAX_WORDS);
    out
}

/// A completion item for `word`: inserted as it is, marked as a word.
pub fn item(word: String) -> crate::lsp::CompletionItem {
    crate::lsp::CompletionItem {
        label: word,
        kind: Some(lsp_types::CompletionItemKind::TEXT),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(String::from).collect()
    }

    #[test]
    fn offers_the_words_in_the_file_but_not_the_one_being_typed() {
        let doc = lines("configuration loader\nconfigu");
        let words = collect(&doc, (1, 7), &[], "configu");
        assert_eq!(words, ["configuration"]);
    }

    #[test]
    fn nearest_words_come_first_then_other_documents() {
        let doc = lines("build_far\n\n\n\nbuild_near\nbu");
        let other = lines("build_other build_near");
        let words = collect(&doc, (5, 2), &[&other], "bu");
        assert_eq!(words, ["build_near", "build_far", "build_other"]);
    }

    #[test]
    fn matching_ignores_case_and_keeps_the_word_s_own_case() {
        let doc = lines("BUILD_DIRECTORY := out\n$(bui");
        assert_eq!(collect(&doc, (1, 5), &[], "bui"), ["BUILD_DIRECTORY"]);
    }

    #[test]
    fn an_empty_prefix_offers_every_word_once() {
        let doc = lines("alpha beta alpha\n");
        assert_eq!(collect(&doc, (1, 0), &[], ""), ["alpha", "beta"]);
    }

    #[test]
    fn numbers_unrelated_words_and_the_prefix_itself_are_not_offered() {
        let doc = lines("port 8080 portal\nport other\npor");
        let words = collect(&doc, (2, 3), &[], "port");
        assert_eq!(words, ["portal"], "not `port` itself, nor `other`");
        assert!(collect(&doc, (2, 3), &[], "80").is_empty());
    }

    #[test]
    fn a_huge_file_is_capped() {
        let doc: Vec<String> = (0..MAX_WORDS + 50).map(|i| format!("w{i}")).collect();
        assert_eq!(collect(&doc, (0, 0), &[], "w").len(), MAX_WORDS);
    }

    #[test]
    fn a_huge_file_is_capped_when_other_documents_match_too() {
        let doc: Vec<String> = (0..MAX_WORDS + 50).map(|i| format!("w{i}")).collect();
        let other = lines("wother");
        assert_eq!(collect(&doc, (0, 0), &[&other], "w").len(), MAX_WORDS);
    }

    #[test]
    fn the_setting_reads_vs_code_s_ids() {
        assert_eq!(WordSuggestions::from_pref("off"), WordSuggestions::Off);
        assert_eq!(
            WordSuggestions::from_pref("currentDocument"),
            WordSuggestions::CurrentDocument
        );
        assert_eq!(
            WordSuggestions::from_pref("allDocuments"),
            WordSuggestions::AllDocuments
        );
        assert_eq!(
            WordSuggestions::from_pref(""),
            WordSuggestions::MatchingDocuments
        );
        assert_eq!(
            WordSuggestions::from_pref("bogus"),
            WordSuggestions::MatchingDocuments
        );
    }
}
