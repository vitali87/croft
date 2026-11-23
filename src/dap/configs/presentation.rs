//! `presentation` blocks: hiding an entry, its group and order, the malformed shapes
//! reported, and the picker's order, as VS Code sorts it.

use super::*;

/// A `presentation` block, as declared on a configuration OR a compound
/// (#318). VS Code reads the same three keys in both places and sorts the two
/// kinds together in one list, so croft parses it in both too.
// `Eq` is intentionally not derived: `presentation.order` is a `f64`, and
// floats break total equality. (`ResolvedConfig` below DOES derive `Eq` - it
// carries no float - so it is not the precedent this looks like.) None of
// these is ever a map key, so `PartialEq` is all the call sites and tests
// need.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Presentation {
    /// Do not list this entry in the picker.
    pub hidden: bool,
    /// The name entries are grouped under. Groups sort against each other by
    /// name; entries carrying one sort ahead of entries that do not.
    pub group: Option<String>,
    /// Position within the group, ascending. An entry with no numeric order
    /// sorts after the ones that have it.
    ///
    /// `f64` rather than an integer because VS Code's schema says `number`
    /// and its comparator subtracts one from the other, so `1.5` is a legal
    /// position between two neighbours - not the typo that a non-number is.
    pub order: Option<f64>,
}

/// Read a `presentation` block, if the entry declares one at all.
///
/// `None` only for ABSENT or unreadable. An empty block is still a block:
/// VS Code's comparator tests the object itself for truthiness (`if
/// (!first.presentation) … return 1`), so `{}` is truthy there and does sort
/// ahead of an entry carrying no block. Returning `None` for it would place
/// those rows differently than VS Code places them, which is the one thing a
/// transcription must not do.
///
/// The INNER checks stay truthiness-based, matching the same comparator: an
/// empty-string `group` is no group.
pub fn parse_presentation(obj: &Map<String, Value>) -> Option<Presentation> {
    let p = obj.get("presentation")?.as_object()?;
    let hidden = p.get("hidden").and_then(Value::as_bool).unwrap_or(false);
    let group = p
        .get("group")
        .and_then(Value::as_str)
        .filter(|g| !g.is_empty())
        .map(str::to_string);
    let order = p.get("order").and_then(Value::as_f64);
    Some(Presentation {
        hidden,
        group,
        order,
    })
}

/// The malformed `presentation` shapes this entry declares, named with their
/// parent so they can be found in the file (#318).
///
/// A value croft cannot read is a TYPO, and `{"order": "1"}` is the same
/// mistake as `{"hidden": "true"}`: quoted where it should be bare. Dropping
/// it silently would leave the user with a picker that ignores the ordering
/// they wrote and nothing to explain why.
///
/// Only COMPOUNDS act on this. A configuration has no refusal path to hang it
/// on - it launches directly rather than through a gate - so a config follows
/// VS Code and ignores what it cannot read.
pub fn malformed_presentation_keys(obj: &Map<String, Value>) -> Vec<&'static str> {
    let Some(value) = obj.get("presentation") else {
        return Vec::new();
    };
    // A key that REQUESTS NOTHING is honoured by doing nothing, and naming it
    // in a refusal would report a limitation croft does not have. `null`,
    // `false` and `""` are that shape; this list is the same one
    // `asks_for_something` applies to the sibling keys, kept in step with it
    // on purpose.
    if value.is_null() || value == &Value::Bool(false) || value.as_str().is_some_and(str::is_empty)
    {
        return Vec::new();
    }
    let Some(p) = value.as_object() else {
        return vec!["presentation"];
    };
    let mut out = Vec::new();
    for (key, path, well_formed) in [
        (
            "hidden",
            "presentation.hidden",
            Value::is_boolean as fn(&Value) -> bool,
        ),
        (
            "group",
            "presentation.group",
            Value::is_string as fn(&Value) -> bool,
        ),
        (
            "order",
            "presentation.order",
            Value::is_number as fn(&Value) -> bool,
        ),
    ] {
        if p.get(key).is_some_and(|v| !v.is_null() && !well_formed(v)) {
            out.push(path);
        }
    }
    out
}

/// Filter out hidden entries and sort the rest the way VS Code's
/// `getVisibleAndSorted` does (#318), so a launch.json ordered for VS Code
/// reads the same here.
///
/// The comparator is transcribed from `debugUtils.ts` rather than invented,
/// because every one of its rules is a decision croft would otherwise get
/// subtly wrong:
///
/// - an entry WITH a `presentation` sorts ahead of one without, so adding
///   `{"order": 1}` to a single entry lifts it above untouched neighbours;
/// - within those, an entry with a `group` sorts ahead of one without;
/// - groups compare by NAME, so grouping is alphabetical rather than
///   first-seen;
/// - inside a group, a numeric `order` sorts ahead of a missing one.
///
/// The sort is stable, matching JS, which is what keeps a workspace that
/// declares no `presentation` at all in exactly its file order - the case
/// that must not move.
///
/// One deliberate divergence: VS Code compares group names with
/// `localeCompare`, which is locale-aware collation; croft compares them
/// bytewise. The two agree on ASCII group names and can disagree on accented
/// ones, and matching it properly would mean taking a collation dependency
/// for the ordering of a picker.
pub fn visible_and_sorted<T>(
    items: Vec<T>,
    presentation: impl Fn(&T) -> Option<&Presentation>,
) -> Vec<T> {
    let mut kept: Vec<T> = items
        .into_iter()
        .filter(|i| !presentation(i).is_some_and(|p| p.hidden))
        .collect();
    kept.sort_by(|a, b| {
        use std::cmp::Ordering;
        let (Some(first), Some(second)) = (presentation(a), presentation(b)) else {
            return match (presentation(a), presentation(b)) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                _ => unreachable!("both-Some is handled by the outer let-else"),
            };
        };
        match (&first.group, &second.group) {
            (None, None) => compare_orders(first.order, second.order),
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(g1), Some(g2)) if g1 != g2 => g1.cmp(g2),
            _ => compare_orders(first.order, second.order),
        }
    });
    kept
}

/// `order` against `order`: a declared one wins over a missing one, and two
/// missing ones tie (leaving the stable sort to keep file order).
pub(super) fn compare_orders(first: Option<f64>, second: Option<f64>) -> std::cmp::Ordering {
    match (first, second) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        // `total_cmp` rather than `partial_cmp` with a fallback: JSON cannot
        // carry a NaN, but a comparator that answered `Equal` for one would
        // break transitivity, which `sort_by` panics on. A total order has no
        // such arm to get wrong. It orders `-0.0` before `0.0` where VS Code's
        // subtraction calls them equal; `"order": -0.0` is expressible and
        // nothing else about the two differs.
        (Some(a), Some(b)) => a.total_cmp(&b),
    }
}

impl Compound {
    /// Whether the compound asked not to be listed (#318).
    pub fn hidden(&self) -> bool {
        self.presentation.as_ref().is_some_and(|p| p.hidden)
    }
}

impl DebugConfig {
    /// Whether the configuration asked not to be listed (#318). Same question
    /// as [`Compound::hidden`], and the picker's badge asks it of both kinds
    /// so the count and the list cannot disagree.
    pub fn hidden(&self) -> bool {
        self.presentation.as_ref().is_some_and(|p| p.hidden)
    }
}
