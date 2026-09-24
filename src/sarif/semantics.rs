//! The SARIF 2.1.0 rules a viewer applies on top of the raw objects.
//!
//! Each function cites the spec section it implements, since the defaults are
//! easy to get subtly wrong: a result with no `level` is not necessarily a
//! warning, and a result with suppressions is not necessarily suppressed.

use super::model::{
    Message, MultiformatMessageString, ReportingDescriptor, Run, SarifResult, ToolComponent,
};

/// §3.27.10 `level`, after defaulting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Error,
    Warning,
    Note,
    None,
}

impl Level {
    pub fn parse(s: &str) -> Option<Level> {
        match s {
            "error" => Some(Level::Error),
            "warning" => Some(Level::Warning),
            "note" => Some(Level::Note),
            "none" => Some(Level::None),
            _ => None,
        }
    }
}

/// §3.27.9 `kind`, after defaulting (absent means `fail`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Fail,
    Pass,
    Open,
    Review,
    NotApplicable,
    Informational,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "fail" => Some(Kind::Fail),
            "pass" => Some(Kind::Pass),
            "open" => Some(Kind::Open),
            "review" => Some(Kind::Review),
            "notApplicable" => Some(Kind::NotApplicable),
            "informational" => Some(Kind::Informational),
            _ => None,
        }
    }
}

/// §3.27.24 `baselineState`; `Unspecified` when the log carries none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BaselineState {
    New,
    Unchanged,
    Updated,
    Absent,
    Unspecified,
}

/// §3.27.23 / §3.35.3: what a result's `suppressions` array amounts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SuppressionState {
    /// `suppressions` absent: the producer does not track suppression.
    Unknown,
    /// Present and empty, or every entry rejected.
    NotSuppressed,
    /// At least one entry is still under review.
    UnderReview,
    /// Every entry accepted (a missing `status` means accepted).
    Suppressed,
}

pub fn result_kind(result: &SarifResult) -> Kind {
    result
        .kind
        .as_deref()
        .and_then(Kind::parse)
        .unwrap_or(Kind::Fail)
}

/// §3.27.10: `level` if given; otherwise `none` for a non-`fail` kind; else
/// the rule's `defaultConfiguration.level`; else `warning`.
pub fn effective_level(result: &SarifResult, rule: Option<&ReportingDescriptor>) -> Level {
    if let Some(level) = result.level.as_deref().and_then(Level::parse) {
        return level;
    }
    if result_kind(result) != Kind::Fail {
        return Level::None;
    }
    rule.and_then(|r| r.default_configuration.as_ref())
        .and_then(|c| c.level.as_deref())
        .and_then(Level::parse)
        .unwrap_or(Level::Warning)
}

pub fn baseline_state(result: &SarifResult) -> BaselineState {
    match result.baseline_state.as_deref() {
        Some("new") => BaselineState::New,
        Some("unchanged") => BaselineState::Unchanged,
        Some("updated") => BaselineState::Updated,
        Some("absent") => BaselineState::Absent,
        _ => BaselineState::Unspecified,
    }
}

pub fn suppression_state(result: &SarifResult) -> SuppressionState {
    let Some(list) = result.suppressions.as_ref() else {
        return SuppressionState::Unknown;
    };
    let mut accepted = false;
    for s in list {
        match s.status.as_deref() {
            Some("underReview") => return SuppressionState::UnderReview,
            Some("rejected") => {}
            _ => accepted = true,
        }
    }
    if accepted {
        SuppressionState::Suppressed
    } else {
        SuppressionState::NotSuppressed
    }
}

/// The tool component a result's rule lives in: `rule.toolComponent` picks an
/// extension by index, guid or name; everything else is the driver.
pub fn rule_component<'a>(run: &'a Run, result: &SarifResult) -> &'a ToolComponent {
    let Some(tc) = result.rule.as_ref().and_then(|r| r.tool_component.as_ref()) else {
        return &run.tool.driver;
    };
    let driver = &run.tool.driver;
    let ext = &run.tool.extensions;
    if let Some(i) = tc.index {
        return usize::try_from(i)
            .ok()
            .and_then(|i| ext.get(i))
            .unwrap_or(driver);
    }
    if let Some(g) = tc.guid.as_deref() {
        if driver.guid.as_deref() == Some(g) {
            return driver;
        }
        if let Some(e) = ext.iter().find(|e| e.guid.as_deref() == Some(g)) {
            return e;
        }
    }
    if let Some(n) = tc.name.as_deref()
        && driver.name != n
        && let Some(e) = ext.iter().find(|e| e.name == n)
    {
        return e;
    }
    driver
}

/// §3.27.5–7, §3.52.3–5: resolve the rule by index first, then by id. A
/// hierarchical id (`CA2000/1`) falls back to its leading components.
pub fn rule_for<'a>(run: &'a Run, result: &SarifResult) -> Option<&'a ReportingDescriptor> {
    let rules = &rule_component(run, result).rules;
    let index = result
        .rule
        .as_ref()
        .and_then(|r| r.index)
        .or(result.rule_index)
        .and_then(|i| usize::try_from(i).ok());
    if let Some(rule) = index.and_then(|i| rules.get(i)) {
        return Some(rule);
    }
    let id = result
        .rule_id
        .as_deref()
        .or_else(|| result.rule.as_ref().and_then(|r| r.id.as_deref()))?;
    let mut candidate = id;
    loop {
        if let Some(rule) = rules.iter().find(|r| r.id == candidate) {
            return Some(rule);
        }
        candidate = &candidate[..candidate.rfind('/')?];
    }
}

/// The rule id to display: `ruleId`, else `rule.id`, else the resolved rule's id.
pub fn rule_id(run: &Run, result: &SarifResult) -> Option<String> {
    result
        .rule_id
        .clone()
        .or_else(|| result.rule.as_ref().and_then(|r| r.id.clone()))
        .or_else(|| rule_for(run, result).map(|r| r.id.clone()))
}

fn lookup<'a>(
    id: &str,
    rule: Option<&'a ReportingDescriptor>,
    component: Option<&'a ToolComponent>,
) -> Option<&'a MultiformatMessageString> {
    rule.and_then(|r| r.message_strings.get(id))
        .or_else(|| component.and_then(|c| c.global_message_strings.get(id)))
}

/// §3.11.7–11: the plain text of a message. `text` wins; otherwise `id` is
/// looked up in the rule's `messageStrings`, then the component's
/// `globalMessageStrings`. `{n}` placeholders take `arguments[n]`; `{{` and
/// `}}` are literal braces.
pub fn message_text(
    msg: &Message,
    rule: Option<&ReportingDescriptor>,
    component: Option<&ToolComponent>,
) -> String {
    let template = msg.text.clone().or_else(|| {
        let id = msg.id.as_deref()?;
        Some(
            lookup(id, rule, component)
                .map(|m| m.text.clone())
                .unwrap_or_else(|| id.to_string()),
        )
    });
    substitute(&template.unwrap_or_default(), &msg.arguments)
}

/// Like [`message_text`] but preferring `markdown` where the source has one.
pub fn message_markdown(
    msg: &Message,
    rule: Option<&ReportingDescriptor>,
    component: Option<&ToolComponent>,
) -> String {
    if let Some(md) = &msg.markdown {
        return substitute(md, &msg.arguments);
    }
    if msg.text.is_none()
        && let Some(found) = msg.id.as_deref().and_then(|id| lookup(id, rule, component))
        && let Some(md) = &found.markdown
    {
        return substitute(md, &msg.arguments);
    }
    message_text(msg, rule, component)
}

/// §3.11.5: replace `{n}` with `args[n]`; `{{`/`}}` are literal braces. A
/// placeholder with no matching argument is left as written.
fn substitute(template: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        if tail.starts_with('{')
            && let Some(close) = tail.find('}')
            && let Ok(n) = tail[1..close].parse::<usize>()
            && let Some(arg) = args.get(n)
        {
            out.push_str(arg);
            rest = &tail[close + 1..];
            continue;
        }
        out.push_str(&tail[..1]);
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

/// A piece of message text: plain, or an embedded link (§3.11.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    /// `[text](n)`: a link to the location whose `id` is `n` in this result's
    /// `relatedLocations` (or `locations`).
    LocationLink { text: String, id: i64 },
    /// `[text](uri)` for any other target.
    UriLink { text: String, uri: String },
}

/// Split message text into plain runs and embedded links. `\[` and `\]` are
/// literal brackets and never start or end a link.
pub fn segments(text: &str) -> Vec<Segment> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' if matches!(chars.get(i + 1), Some('[' | ']')) => {
                plain.push(chars[i + 1]);
                i += 2;
            }
            '[' => match parse_link(&chars, i) {
                Some((seg, next)) => {
                    if !plain.is_empty() {
                        out.push(Segment::Text(std::mem::take(&mut plain)));
                    }
                    out.push(seg);
                    i = next;
                }
                None => {
                    plain.push('[');
                    i += 1;
                }
            },
            c => {
                plain.push(c);
                i += 1;
            }
        }
    }
    if !plain.is_empty() {
        out.push(Segment::Text(plain));
    }
    out
}

/// Parse `[text](target)` starting at the `[` at `open`; returns the segment
/// and the index just past `)`.
fn parse_link(chars: &[char], open: usize) -> Option<(Segment, usize)> {
    let mut text = String::new();
    let mut i = open + 1;
    loop {
        match chars.get(i)? {
            '\\' if matches!(chars.get(i + 1), Some('[' | ']')) => {
                text.push(chars[i + 1]);
                i += 2;
            }
            '[' => return None,
            ']' => break,
            c => {
                text.push(*c);
                i += 1;
            }
        }
    }
    if chars.get(i + 1) != Some(&'(') {
        return None;
    }
    let start = i + 2;
    let close = start + chars[start..].iter().position(|&c| c == ')')?;
    let target: String = chars[start..close].iter().collect();
    if target.is_empty() {
        return None;
    }
    let seg = match target.parse::<i64>() {
        Ok(id) => Segment::LocationLink { text, id },
        Err(_) => Segment::UriLink { text, uri: target },
    };
    Some((seg, close + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sarif::model::SarifLog;

    fn run(json: &str) -> Run {
        let log: SarifLog = serde_json::from_str(json).unwrap();
        log.runs.into_iter().next().unwrap()
    }

    fn result(json: &str) -> SarifResult {
        serde_json::from_str(json).unwrap()
    }

    fn rule_with_default(level: &str) -> ReportingDescriptor {
        serde_json::from_str(&format!(
            r#"{{"id":"R1","defaultConfiguration":{{"level":"{level}"}}}}"#
        ))
        .unwrap()
    }

    // ── kind / level ────────────────────────────────────────────────────

    #[test]
    fn kind_defaults_to_fail() {
        assert_eq!(result_kind(&result("{}")), Kind::Fail);
        assert_eq!(result_kind(&result(r#"{"kind":"open"}"#)), Kind::Open);
        assert_eq!(
            result_kind(&result(r#"{"kind":"notApplicable"}"#)),
            Kind::NotApplicable
        );
    }

    #[test]
    fn explicit_level_wins() {
        let r = result(r#"{"level":"note"}"#);
        assert_eq!(
            effective_level(&r, Some(&rule_with_default("error"))),
            Level::Note
        );
    }

    #[test]
    fn missing_level_takes_rule_default() {
        let r = result("{}");
        assert_eq!(
            effective_level(&r, Some(&rule_with_default("error"))),
            Level::Error
        );
    }

    #[test]
    fn missing_level_without_rule_is_warning() {
        assert_eq!(effective_level(&result("{}"), None), Level::Warning);
    }

    #[test]
    fn non_fail_kind_without_level_is_none() {
        let r = result(r#"{"kind":"pass"}"#);
        assert_eq!(
            effective_level(&r, Some(&rule_with_default("error"))),
            Level::None
        );
    }

    #[test]
    fn unrecognised_level_string_falls_through_to_default() {
        let r = result(r#"{"level":"critical"}"#);
        assert_eq!(effective_level(&r, None), Level::Warning);
    }

    // ── baseline / suppression ──────────────────────────────────────────

    #[test]
    fn baseline_states() {
        assert_eq!(baseline_state(&result("{}")), BaselineState::Unspecified);
        for (s, want) in [
            ("new", BaselineState::New),
            ("unchanged", BaselineState::Unchanged),
            ("updated", BaselineState::Updated),
            ("absent", BaselineState::Absent),
        ] {
            let r = result(&format!(r#"{{"baselineState":"{s}"}}"#));
            assert_eq!(baseline_state(&r), want, "{s}");
        }
    }

    #[test]
    fn suppression_absent_is_unknown() {
        assert_eq!(suppression_state(&result("{}")), SuppressionState::Unknown);
    }

    #[test]
    fn suppression_empty_is_not_suppressed() {
        assert_eq!(
            suppression_state(&result(r#"{"suppressions":[]}"#)),
            SuppressionState::NotSuppressed
        );
    }

    #[test]
    fn suppression_without_status_is_accepted() {
        assert_eq!(
            suppression_state(&result(r#"{"suppressions":[{"kind":"inSource"}]}"#)),
            SuppressionState::Suppressed
        );
    }

    #[test]
    fn suppression_under_review_outranks_accepted() {
        let r = result(
            r#"{"suppressions":[{"kind":"external","status":"accepted"},{"kind":"external","status":"underReview"}]}"#,
        );
        assert_eq!(suppression_state(&r), SuppressionState::UnderReview);
    }

    #[test]
    fn suppression_all_rejected_is_not_suppressed() {
        let r = result(r#"{"suppressions":[{"kind":"external","status":"rejected"}]}"#);
        assert_eq!(suppression_state(&r), SuppressionState::NotSuppressed);
    }

    #[test]
    fn suppression_rejected_plus_accepted_is_suppressed() {
        // §3.35.3: a rejected suppression is simply not in force; the
        // accepted one still is.
        let r = result(
            r#"{"suppressions":[{"kind":"external","status":"rejected"},{"kind":"inSource"}]}"#,
        );
        assert_eq!(suppression_state(&r), SuppressionState::Suppressed);
    }

    // ── rule resolution ─────────────────────────────────────────────────

    const RULES: &str = r#"{"runs":[{"tool":{
        "driver":{"name":"d","rules":[{"id":"A1","name":"alpha"},{"id":"B2","name":"beta"}]},
        "extensions":[{"name":"ext","guid":"G-1","rules":[{"id":"X9","name":"ext-rule"}]}]
    }}]}"#;

    #[test]
    fn rule_by_rule_index() {
        let run = run(RULES);
        let r = result(r#"{"ruleId":"ignored","ruleIndex":1}"#);
        assert_eq!(rule_for(&run, &r).unwrap().id, "B2");
    }

    #[test]
    fn rule_by_rule_id() {
        let run = run(RULES);
        let r = result(r#"{"ruleId":"A1"}"#);
        assert_eq!(rule_for(&run, &r).unwrap().name.as_deref(), Some("alpha"));
    }

    #[test]
    fn rule_by_hierarchical_id_falls_back_to_parent() {
        let run = run(RULES);
        let r = result(r#"{"ruleId":"A1/sub/leaf"}"#);
        assert_eq!(rule_for(&run, &r).unwrap().id, "A1");
    }

    #[test]
    fn rule_reference_into_extension_by_index() {
        let run = run(RULES);
        let r = result(r#"{"rule":{"index":0,"toolComponent":{"index":0}}}"#);
        assert_eq!(rule_for(&run, &r).unwrap().id, "X9");
        assert_eq!(rule_component(&run, &r).name, "ext");
    }

    #[test]
    fn rule_reference_into_extension_by_guid_and_id() {
        let run = run(RULES);
        let r = result(r#"{"rule":{"id":"X9","toolComponent":{"guid":"G-1"}}}"#);
        assert_eq!(rule_for(&run, &r).unwrap().name.as_deref(), Some("ext-rule"));
    }

    #[test]
    fn rule_reference_into_extension_by_name() {
        let run = run(RULES);
        let r = result(r#"{"rule":{"id":"X9","toolComponent":{"name":"ext"}}}"#);
        assert_eq!(rule_for(&run, &r).unwrap().id, "X9");
    }

    #[test]
    fn out_of_range_index_falls_back_to_id() {
        let run = run(RULES);
        let r = result(r#"{"ruleId":"A1","ruleIndex":99}"#);
        assert_eq!(rule_for(&run, &r).unwrap().id, "A1");
    }

    #[test]
    fn unknown_rule_is_none() {
        let run = run(RULES);
        assert!(rule_for(&run, &result(r#"{"ruleId":"ZZ"}"#)).is_none());
    }

    #[test]
    fn rule_id_display_prefers_rule_id_then_reference_then_resolved() {
        let run = run(RULES);
        assert_eq!(rule_id(&run, &result(r#"{"ruleId":"Q"}"#)).as_deref(), Some("Q"));
        assert_eq!(
            rule_id(&run, &result(r#"{"rule":{"id":"R"}}"#)).as_deref(),
            Some("R")
        );
        assert_eq!(
            rule_id(&run, &result(r#"{"ruleIndex":0}"#)).as_deref(),
            Some("A1")
        );
        assert_eq!(rule_id(&run, &result("{}")), None);
    }

    // ── messages ────────────────────────────────────────────────────────

    fn msg(json: &str) -> Message {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn text_with_arguments() {
        let m = msg(r#"{"text":"Variable '{0}' shadows '{1}'.","arguments":["x","y"]}"#);
        assert_eq!(message_text(&m, None, None), "Variable 'x' shadows 'y'.");
    }

    #[test]
    fn doubled_braces_are_literal() {
        let m = msg(r#"{"text":"use {{0}} not {0}","arguments":["z"]}"#);
        assert_eq!(message_text(&m, None, None), "use {0} not z");
    }

    #[test]
    fn missing_argument_leaves_placeholder() {
        let m = msg(r#"{"text":"a {0} b {3}","arguments":["x"]}"#);
        assert_eq!(message_text(&m, None, None), "a x b {3}");
    }

    #[test]
    fn id_looks_up_rule_message_strings() {
        let rule: ReportingDescriptor = serde_json::from_str(
            r#"{"id":"R","messageStrings":{"default":{"text":"'{0}' is unused.","markdown":"`{0}` is unused."}}}"#,
        )
        .unwrap();
        let m = msg(r#"{"id":"default","arguments":["foo"]}"#);
        assert_eq!(message_text(&m, Some(&rule), None), "'foo' is unused.");
        assert_eq!(message_markdown(&m, Some(&rule), None), "`foo` is unused.");
    }

    #[test]
    fn id_falls_back_to_global_message_strings() {
        let comp: ToolComponent = serde_json::from_str(
            r#"{"name":"t","globalMessageStrings":{"g":{"text":"global {0}"}}}"#,
        )
        .unwrap();
        let rule: ReportingDescriptor = serde_json::from_str(r#"{"id":"R"}"#).unwrap();
        let m = msg(r#"{"id":"g","arguments":["hit"]}"#);
        assert_eq!(message_text(&m, Some(&rule), Some(&comp)), "global hit");
    }

    #[test]
    fn markdown_falls_back_to_text() {
        let m = msg(r#"{"text":"plain"}"#);
        assert_eq!(message_markdown(&m, None, None), "plain");
        let m = msg(r#"{"text":"plain","markdown":"**rich**"}"#);
        assert_eq!(message_markdown(&m, None, None), "**rich**");
        assert_eq!(message_text(&m, None, None), "plain");
    }

    #[test]
    fn unresolvable_id_yields_the_id() {
        let m = msg(r#"{"id":"nowhere"}"#);
        assert_eq!(message_text(&m, None, None), "nowhere");
    }

    // ── embedded links ──────────────────────────────────────────────────

    #[test]
    fn segments_plain_text() {
        assert_eq!(segments("just text"), vec![Segment::Text("just text".into())]);
    }

    #[test]
    fn segments_location_and_uri_links() {
        assert_eq!(
            segments("tainted by [this input](1) see [docs](https://x.y/z)."),
            vec![
                Segment::Text("tainted by ".into()),
                Segment::LocationLink {
                    text: "this input".into(),
                    id: 1
                },
                Segment::Text(" see ".into()),
                Segment::UriLink {
                    text: "docs".into(),
                    uri: "https://x.y/z".into()
                },
                Segment::Text(".".into()),
            ]
        );
    }

    #[test]
    fn segments_escaped_brackets_are_literal() {
        assert_eq!(
            segments(r"array\[0\](1) is fine"),
            vec![Segment::Text("array[0](1) is fine".into())]
        );
    }

    #[test]
    fn segments_unclosed_link_is_text() {
        assert_eq!(
            segments("broken [link(1) and [x]("),
            vec![Segment::Text("broken [link(1) and [x](".into())]
        );
    }

    #[test]
    fn segments_escaped_bracket_inside_link_text() {
        assert_eq!(
            segments(r"[a\]b](2)"),
            vec![Segment::LocationLink {
                text: "a]b".into(),
                id: 2
            }]
        );
    }
}
