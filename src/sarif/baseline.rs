//! Comparing a log against a baseline log (§3.27.24 `baselineState`), for
//! logs whose producer did not record one: which results are new, which
//! persist unchanged or moved, and which baseline results are gone.
//!
//! Matching follows the spec's guidance on result identity (§3.27.16–17):
//! results with `fingerprints` match on a shared fingerprint; otherwise on a
//! shared `partialFingerprints` entry; otherwise on rule, file and message,
//! so a finding that only drifted by a few lines still counts as the same
//! one. Each baseline result matches at most one current result.

use super::model::SarifLog;
use super::resolve::location_parts;
use super::semantics::{self as sem, BaselineState};
use std::collections::BTreeMap;

/// What [`compare`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comparison {
    /// The state of every current result, indexed `[run][result]`.
    pub current: Vec<Vec<BaselineState>>,
    /// Baseline results with no current match, as `(run, result)` indices
    /// into the baseline log.
    pub absent: Vec<(usize, usize)>,
}

/// What identifies one result across two logs.
struct Identity {
    rule: String,
    uri: String,
    line: i64,
    message: String,
    fingerprints: BTreeMap<String, String>,
    partial: BTreeMap<String, String>,
}

fn identities(log: &SarifLog) -> Vec<(usize, usize, Identity)> {
    let mut out = Vec::new();
    for (ri, run) in log.runs.iter().enumerate() {
        for (i, result) in run.results.iter().flatten().enumerate() {
            let rule = sem::rule_for(run, result);
            let component = sem::rule_component(run, result);
            let physical = result
                .locations
                .first()
                .and_then(|l| l.physical_location.as_ref());
            let uri = physical
                .and_then(|p| p.artifact_location.as_ref())
                .and_then(|a| location_parts(run, a))
                .map(|(uri, base)| {
                    format!(
                        "{}{uri}",
                        base.map(|b| format!("%{b}%/")).unwrap_or_default()
                    )
                })
                .unwrap_or_default();
            out.push((
                ri,
                i,
                Identity {
                    rule: sem::rule_id(run, result).unwrap_or_default(),
                    uri,
                    line: physical
                        .and_then(|p| p.region.as_ref())
                        .and_then(|r| r.start_line)
                        .unwrap_or(0),
                    message: sem::message_text(&result.message, rule, Some(component)),
                    fingerprints: result.fingerprints.clone(),
                    partial: result.partial_fingerprints.clone(),
                },
            ));
        }
    }
    out
}

/// Whether two maps agree on a key they share: `None` when they share none.
fn agree(a: &BTreeMap<String, String>, b: &BTreeMap<String, String>) -> Option<bool> {
    let mut shared = a
        .iter()
        .filter_map(|(k, v)| b.get(k).map(|w| v == w))
        .peekable();
    shared.peek()?;
    Some(shared.any(|same| same))
}

/// Whether `a` and `b` are the same finding, by the strongest evidence both
/// carry: full fingerprints, then partial ones, then rule, file and message.
fn same(a: &Identity, b: &Identity) -> bool {
    if a.rule != b.rule {
        return false;
    }
    if let Some(v) = agree(&a.fingerprints, &b.fingerprints) {
        return v;
    }
    if let Some(v) = agree(&a.partial, &b.partial) {
        return v;
    }
    a.uri == b.uri && a.message == b.message
}

/// Compare `current` against `baseline`.
pub fn compare(current: &SarifLog, baseline: &SarifLog) -> Comparison {
    let now = identities(current);
    let before = identities(baseline);
    let mut out = Comparison {
        current: current
            .runs
            .iter()
            .map(|r| vec![BaselineState::New; r.results.as_ref().map_or(0, Vec::len)])
            .collect(),
        absent: Vec::new(),
    };
    let mut taken = vec![false; before.len()];
    let mut matched = vec![false; now.len()];
    // Exact positions first, so a duplicate that stayed put is the one
    // called unchanged and its moved twin is the new one.
    for exact in [true, false] {
        for (n, (ri, i, id)) in now.iter().enumerate() {
            if matched[n] {
                continue;
            }
            let hit = before.iter().enumerate().position(|(b, (_, _, old))| {
                !taken[b] && same(id, old) && (!exact || (id.uri == old.uri && id.line == old.line))
            });
            if let Some(b) = hit {
                taken[b] = true;
                matched[n] = true;
                out.current[*ri][*i] = if exact {
                    BaselineState::Unchanged
                } else {
                    BaselineState::Updated
                };
            }
        }
    }
    out.absent = before
        .iter()
        .zip(&taken)
        .filter(|(_, t)| !**t)
        .map(|((ri, i, _), _)| (*ri, *i))
        .collect();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sarif::load::parse_log;

    fn log(results: &str) -> SarifLog {
        parse_log(&format!(
            r#"{{"version":"2.1.0","runs":[{{"tool":{{"driver":{{"name":"T"}}}},"results":[{results}]}}]}}"#
        ))
        .unwrap()
    }

    fn at(rule: &str, file: &str, line: i64, msg: &str, extra: &str) -> String {
        format!(
            r#"{{"ruleId":"{rule}","message":{{"text":"{msg}"}},{extra}"locations":[{{"physicalLocation":{{"artifactLocation":{{"uri":"{file}"}},"region":{{"startLine":{line}}}}}}}]}}"#
        )
    }

    use BaselineState::*;

    #[test]
    fn identical_logs_are_unchanged_with_nothing_absent() {
        let a = log(&[at("r1", "a.js", 3, "m", ""), at("r2", "b.js", 9, "n", "")].join(","));
        let c = compare(&a, &a);
        assert_eq!(c.current, vec![vec![Unchanged, Unchanged]]);
        assert!(c.absent.is_empty());
    }

    #[test]
    fn a_result_only_now_is_new_and_one_only_before_is_absent() {
        let before = log(&[at("r1", "a.js", 3, "m", ""), at("gone", "c.js", 1, "x", "")].join(","));
        let now = log(&[
            at("r1", "a.js", 3, "m", ""),
            at("fresh", "d.js", 2, "y", ""),
        ]
        .join(","));
        let c = compare(&now, &before);
        assert_eq!(c.current, vec![vec![Unchanged, New]]);
        assert_eq!(c.absent, vec![(0, 1)]);
    }

    #[test]
    fn a_fingerprint_match_that_moved_is_updated() {
        let fp = r#""partialFingerprints":{"primaryLocationLineHash":"abc"},"#;
        let before = log(&at("r1", "a.js", 3, "m", fp));
        let now = log(&at("r1", "a.js", 40, "m", fp));
        let c = compare(&now, &before);
        assert_eq!(c.current, vec![vec![Updated]]);
        assert!(c.absent.is_empty());
    }

    #[test]
    fn a_different_fingerprint_is_a_different_result_even_on_the_same_line() {
        let before = log(&at("r1", "a.js", 3, "m", r#""fingerprints":{"v1":"aaa"},"#));
        let now = log(&at("r1", "a.js", 3, "m", r#""fingerprints":{"v1":"bbb"},"#));
        let c = compare(&now, &before);
        assert_eq!(c.current, vec![vec![New]]);
        assert_eq!(c.absent, vec![(0, 0)]);
    }

    #[test]
    fn without_fingerprints_line_drift_is_updated_not_new() {
        let before = log(&at("r1", "a.js", 3, "m", ""));
        let now = log(&at("r1", "a.js", 5, "m", ""));
        assert_eq!(compare(&now, &before).current, vec![vec![Updated]]);
    }

    #[test]
    fn each_baseline_result_matches_one_current_result() {
        let before = log(&at("r1", "a.js", 3, "m", ""));
        let now = log(&[at("r1", "a.js", 3, "m", ""), at("r1", "a.js", 8, "m", "")].join(","));
        assert_eq!(compare(&now, &before).current, vec![vec![Unchanged, New]]);
    }
}
