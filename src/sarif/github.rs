//! GitHub code scanning (#577): the analyses a repository has, one
//! analysis as a SARIF log, and dismissing an alert — as `gh` arguments and
//! parsers, so the calls themselves stay with the caller.
//!
//! Every endpoint uses `gh`'s `{owner}/{repo}` placeholders, which `gh`
//! fills from the repository it runs in.

use super::model::SarifResult;

/// One code scanning analysis, as the analyses list reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analysis {
    pub id: u64,
    pub git_ref: String,
    pub commit_sha: String,
    pub tool: String,
    pub category: String,
    pub created_at: String,
    pub results_count: u64,
    /// Non-empty when the upload failed processing.
    pub error: String,
}

/// One open or dismissed alert, enough to find it from a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub number: u64,
    pub rule_id: String,
    pub path: String,
    pub start_line: i64,
    pub state: String,
}

/// Why an alert is dismissed: the three reasons the API accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DismissReason {
    FalsePositive,
    WontFix,
    UsedInTests,
}

impl DismissReason {
    pub const ALL: [DismissReason; 3] = [
        DismissReason::FalsePositive,
        DismissReason::WontFix,
        DismissReason::UsedInTests,
    ];

    /// The API's spelling.
    pub fn api(self) -> &'static str {
        match self {
            DismissReason::FalsePositive => "false positive",
            DismissReason::WontFix => "won't fix",
            DismissReason::UsedInTests => "used in tests",
        }
    }
}

/// Percent-encode a query value: everything but RFC 3986 unreserved bytes.
fn encode(v: &str) -> String {
    let mut out = String::new();
    for b in v.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn with_ref(mut url: String, git_ref: Option<&str>) -> String {
    if let Some(r) = git_ref {
        url.push_str("&ref=");
        url.push_str(&encode(r));
    }
    url
}

/// `gh` arguments listing the most recent analyses, optionally for one ref.
/// One page only: the newest analyses are the ones worth opening.
pub fn analyses_args(git_ref: Option<&str>) -> Vec<String> {
    vec![
        String::from("api"),
        with_ref(
            String::from("repos/{owner}/{repo}/code-scanning/analyses?per_page=30"),
            git_ref,
        ),
    ]
}

/// A JSON array from the API, or the message of the error object it sent.
fn api_array(json: &str) -> Result<Vec<serde_json::Value>, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("unreadable reply: {e}"))?;
    match v {
        serde_json::Value::Array(a) => Ok(a),
        other => Err(other
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unexpected reply")
            .to_string()),
    }
}

fn text(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

/// The analyses list, or the API's own message when it answered an error
/// object (no code scanning, a missing scope).
pub fn parse_analyses(json: &str) -> Result<Vec<Analysis>, String> {
    Ok(api_array(json)?
        .iter()
        .map(|a| {
            let tool = a.get("tool").cloned().unwrap_or_default();
            let name = text(&tool, "name");
            let version = text(&tool, "version");
            Analysis {
                id: a.get("id").and_then(|x| x.as_u64()).unwrap_or(0),
                git_ref: text(a, "ref"),
                commit_sha: text(a, "commit_sha"),
                tool: if version.is_empty() {
                    name
                } else {
                    format!("{name} {version}")
                },
                category: text(a, "category"),
                created_at: text(a, "created_at"),
                results_count: a.get("results_count").and_then(|x| x.as_u64()).unwrap_or(0),
                error: text(a, "error"),
            }
        })
        .collect())
}

/// A picker line: "#id ref · tool category · N results · created".
pub fn label(a: &Analysis) -> String {
    let git_ref = a
        .git_ref
        .strip_prefix("refs/heads/")
        .or_else(|| a.git_ref.strip_prefix("refs/"))
        .unwrap_or(&a.git_ref);
    let tool = if a.category.is_empty() {
        a.tool.clone()
    } else {
        format!("{} {}", a.tool, a.category)
    };
    let when: String = a
        .created_at
        .chars()
        .take(16)
        .collect::<String>()
        .replace('T', " ");
    let mut out = format!(
        "#{} {git_ref} \u{b7} {tool} \u{b7} {} result{} \u{b7} {when}",
        a.id,
        a.results_count,
        if a.results_count == 1 { "" } else { "s" }
    );
    if !a.error.is_empty() {
        out.push_str(&format!(" \u{b7} failed: {}", a.error));
    }
    out
}

/// `gh` arguments fetching one analysis as SARIF.
pub fn sarif_args(id: u64) -> Vec<String> {
    vec![
        String::from("api"),
        String::from("-H"),
        String::from("Accept: application/sarif+json"),
        format!("repos/{{owner}}/{{repo}}/code-scanning/analyses/{id}"),
    ]
}

/// `gh` arguments listing alerts, one page, for matching results.
pub fn alerts_args(git_ref: Option<&str>) -> Vec<String> {
    vec![
        String::from("api"),
        with_ref(
            String::from("repos/{owner}/{repo}/code-scanning/alerts?per_page=100"),
            git_ref,
        ),
    ]
}

/// The alerts list, or the API's message.
pub fn parse_alerts(json: &str) -> Result<Vec<Alert>, String> {
    Ok(api_array(json)?
        .iter()
        .map(|a| {
            let loc = a
                .get("most_recent_instance")
                .and_then(|i| i.get("location"))
                .cloned()
                .unwrap_or_default();
            Alert {
                number: a.get("number").and_then(|x| x.as_u64()).unwrap_or(0),
                rule_id: a.get("rule").map(|r| text(r, "id")).unwrap_or_default(),
                path: text(&loc, "path"),
                start_line: loc.get("start_line").and_then(|x| x.as_i64()).unwrap_or(0),
                state: text(a, "state"),
            }
        })
        .collect())
}

/// The alert number GitHub stamped on a result (`github/alertNumber`).
pub fn alert_number(result: &SarifResult) -> Option<u64> {
    result.properties.get("github/alertNumber")?.as_u64()
}

/// The alert a result stands for: same rule, same repository path, same
/// start line.
pub fn match_alert(alerts: &[Alert], rule_id: &str, path: &str, line: i64) -> Option<u64> {
    alerts
        .iter()
        .find(|a| a.rule_id == rule_id && a.path == path && a.start_line == line)
        .map(|a| a.number)
}

/// `gh` arguments dismissing alert `number`; a blank comment is left out.
pub fn dismiss_args(number: u64, reason: DismissReason, comment: &str) -> Vec<String> {
    let mut args: Vec<String> = [
        "api",
        "-X",
        "PATCH",
        &format!("repos/{{owner}}/{{repo}}/code-scanning/alerts/{number}"),
        "-f",
        "state=dismissed",
        "-f",
        &format!("dismissed_reason={}", reason.api()),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if !comment.trim().is_empty() {
        args.push(String::from("-f"));
        args.push(format!("dismissed_comment={}", comment.trim()));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANALYSES: &str = r#"[
      {"id":201,"ref":"refs/heads/main","commit_sha":"abc123","analysis_key":".github/workflows/codeql.yml:analyze",
       "environment":"{}","error":"","category":"/language:rust","created_at":"2026-09-20T10:00:00Z",
       "results_count":7,"rules_count":40,"tool":{"name":"CodeQL","version":"2.19.3"},"deletable":true},
      {"id":200,"ref":"refs/pull/12/merge","commit_sha":"def456","error":"upload failed",
       "category":"","created_at":"2026-09-19T09:00:00Z","results_count":0,"tool":{"name":"Semgrep","version":null}}
    ]"#;

    #[test]
    fn analyses_are_listed_one_page_newest_first_optionally_for_a_ref() {
        assert_eq!(
            analyses_args(None),
            vec![
                "api",
                "repos/{owner}/{repo}/code-scanning/analyses?per_page=30"
            ]
        );
        assert_eq!(
            analyses_args(Some("refs/heads/feat/x y")),
            vec![
                "api",
                "repos/{owner}/{repo}/code-scanning/analyses?per_page=30&ref=refs%2Fheads%2Ffeat%2Fx%20y"
            ]
        );
    }

    #[test]
    fn analyses_parse_with_their_tool_and_error() {
        let list = parse_analyses(ANALYSES).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(
            list[0],
            Analysis {
                id: 201,
                git_ref: "refs/heads/main".into(),
                commit_sha: "abc123".into(),
                tool: "CodeQL 2.19.3".into(),
                category: "/language:rust".into(),
                created_at: "2026-09-20T10:00:00Z".into(),
                results_count: 7,
                error: String::new(),
            }
        );
        assert_eq!(list[1].tool, "Semgrep");
        assert_eq!(list[1].error, "upload failed");
    }

    #[test]
    fn an_api_error_object_is_reported_in_its_own_words() {
        let err = parse_analyses(
            r#"{"message":"no analysis found","documentation_url":"https://docs.github.com"}"#,
        )
        .unwrap_err();
        assert_eq!(err, "no analysis found");
        assert!(parse_analyses("not json").is_err());
    }

    #[test]
    fn a_label_names_the_ref_tool_count_and_date() {
        let list = parse_analyses(ANALYSES).unwrap();
        assert_eq!(
            label(&list[0]),
            "#201 main \u{b7} CodeQL 2.19.3 /language:rust \u{b7} 7 results \u{b7} 2026-09-20 10:00"
        );
        assert_eq!(
            label(&list[1]),
            "#200 pull/12/merge \u{b7} Semgrep \u{b7} 0 results \u{b7} 2026-09-19 09:00 \u{b7} failed: upload failed"
        );
    }

    #[test]
    fn an_analysis_is_fetched_as_sarif() {
        assert_eq!(
            sarif_args(201),
            vec![
                "api",
                "-H",
                "Accept: application/sarif+json",
                "repos/{owner}/{repo}/code-scanning/analyses/201"
            ]
        );
    }

    const ALERTS: &str = r#"[
      {"number":7,"state":"open","rule":{"id":"rust/sql-injection"},
       "most_recent_instance":{"location":{"path":"src/db.rs","start_line":42}}},
      {"number":8,"state":"dismissed","rule":{"id":"rust/sql-injection"},
       "most_recent_instance":{"location":{"path":"src/db.rs","start_line":90}}}
    ]"#;

    #[test]
    fn alerts_parse_and_a_result_finds_its_alert_by_rule_path_and_line() {
        assert_eq!(
            alerts_args(Some("refs/heads/main")),
            vec![
                "api",
                "repos/{owner}/{repo}/code-scanning/alerts?per_page=100&ref=refs%2Fheads%2Fmain"
            ]
        );
        let alerts = parse_alerts(ALERTS).unwrap();
        assert_eq!(alerts.len(), 2);
        assert_eq!(alerts[1].state, "dismissed");
        assert_eq!(
            match_alert(&alerts, "rust/sql-injection", "src/db.rs", 90),
            Some(8)
        );
        assert_eq!(
            match_alert(&alerts, "rust/sql-injection", "src/db.rs", 41),
            None
        );
        assert_eq!(match_alert(&alerts, "other", "src/db.rs", 42), None);
    }

    #[test]
    fn a_stamped_alert_number_is_read_from_the_result() {
        let r: SarifResult =
            serde_json::from_str(r#"{"properties":{"github/alertNumber":7}}"#).unwrap();
        assert_eq!(alert_number(&r), Some(7));
        let none: SarifResult = serde_json::from_str("{}").unwrap();
        assert_eq!(alert_number(&none), None);
    }

    #[test]
    fn dismissing_sends_the_reason_in_the_apis_spelling_and_the_comment() {
        assert_eq!(
            dismiss_args(7, DismissReason::WontFix, "tracked in #12"),
            vec![
                "api",
                "-X",
                "PATCH",
                "repos/{owner}/{repo}/code-scanning/alerts/7",
                "-f",
                "state=dismissed",
                "-f",
                "dismissed_reason=won't fix",
                "-f",
                "dismissed_comment=tracked in #12"
            ]
        );
        // No comment, no empty field.
        assert_eq!(dismiss_args(7, DismissReason::FalsePositive, " ").len(), 8);
    }
}
