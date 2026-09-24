//! Parsing a SARIF log from text.
//!
//! Only SARIF 2.1.0 is accepted, identified by `version` or, when a producer
//! left that out, by a `$schema` naming 2.1.0. Anything else is refused with a
//! message that names what was found, so the user knows whether to upgrade the
//! producer or the file.

use super::model::SarifLog;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// Not JSON at all; `line`/`column` are 1-based, from the JSON parser.
    Json {
        line: usize,
        column: usize,
        message: String,
    },
    /// JSON, but not a SARIF log object (for example a top-level array).
    NotSarif(String),
    /// A SARIF log of a version other than 2.1.0.
    UnsupportedVersion(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Json {
                line,
                column,
                message,
            } => write!(f, "invalid JSON at {line}:{column}: {message}"),
            LoadError::NotSarif(why) => write!(f, "not a SARIF log: {why}"),
            LoadError::UnsupportedVersion(v) => write!(
                f,
                "SARIF version {v} is not supported; only 2.1.0 logs can be opened"
            ),
        }
    }
}

pub fn parse_log(text: &str) -> Result<SarifLog, LoadError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| LoadError::Json {
        line: e.line(),
        column: e.column(),
        message: e.to_string(),
    })?;
    let Some(obj) = value.as_object() else {
        return Err(LoadError::NotSarif(
            "the top level is not a JSON object".into(),
        ));
    };
    check_version(
        obj.get("version").and_then(|v| v.as_str()),
        obj.get("$schema").and_then(|v| v.as_str()),
    )?;
    serde_json::from_value(value).map_err(|e| LoadError::NotSarif(e.to_string()))
}

/// `version` decides when present. Without it, a `$schema` URL naming
/// `sarif-2.1.0` (including the `-rtm.N` release-candidate schemas that
/// shipped the final format) is taken as 2.1.0.
fn check_version(version: Option<&str>, schema: Option<&str>) -> Result<(), LoadError> {
    match version {
        Some("2.1.0") => Ok(()),
        Some(other) => Err(LoadError::UnsupportedVersion(other.to_string())),
        None => match schema {
            Some(s) if s.contains("sarif-2.1.0") || s.contains("sarif-schema-2.1.0") => Ok(()),
            _ => Err(LoadError::UnsupportedVersion("(none)".into())),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_210_log() {
        let log = parse_log(
            r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"lint"}},"results":[]}]}"#,
        )
        .unwrap();
        assert_eq!(log.runs.len(), 1);
        assert_eq!(log.runs[0].tool.driver.name, "lint");
    }

    #[test]
    fn unknown_properties_are_ignored() {
        let log = parse_log(
            r#"{"version":"2.1.0","x-custom":1,"runs":[{"tool":{"driver":{"name":"t","weird":[]}},"zzz":{}}]}"#,
        )
        .unwrap();
        assert_eq!(log.runs[0].tool.driver.name, "t");
    }

    #[test]
    fn schema_alone_identifies_210() {
        let log = parse_log(
            r#"{"$schema":"https://json.schemastore.org/sarif-2.1.0.json","runs":[]}"#,
        )
        .unwrap();
        assert!(log.runs.is_empty());
    }

    #[test]
    fn rtm_schema_identifies_210() {
        let log = parse_log(
            r#"{"$schema":"https://schemastore.azurewebsites.net/schemas/json/sarif-2.1.0-rtm.5.json","runs":[]}"#,
        )
        .unwrap();
        assert!(log.runs.is_empty());
    }

    #[test]
    fn older_version_is_refused_by_name() {
        let err = parse_log(r#"{"version":"1.0.0","runs":[]}"#).unwrap_err();
        assert_eq!(err, LoadError::UnsupportedVersion("1.0.0".into()));
        assert!(err.to_string().contains("1.0.0"));
    }

    #[test]
    fn missing_version_and_schema_is_refused() {
        let err = parse_log(r#"{"runs":[]}"#).unwrap_err();
        assert_eq!(err, LoadError::UnsupportedVersion("(none)".into()));
    }

    #[test]
    fn invalid_json_reports_position() {
        let err = parse_log("{\n  \"version\": \"2.1.0\",\n  oops\n}").unwrap_err();
        match err {
            LoadError::Json { line, .. } => assert_eq!(line, 3),
            other => panic!("expected Json error, got {other:?}"),
        }
    }

    #[test]
    fn top_level_array_is_not_sarif() {
        let err = parse_log("[1,2]").unwrap_err();
        assert!(matches!(err, LoadError::NotSarif(_)), "{err:?}");
    }

    #[test]
    fn runs_null_reads_as_empty() {
        // `"runs": null` is legal-ish in the wild (the spec allows null to
        // mean "the tool failed to start"); it must not refuse the log.
        let log = parse_log(r#"{"version":"2.1.0","runs":null}"#).unwrap();
        assert!(log.runs.is_empty());
    }

    #[test]
    fn a_utf8_bom_is_tolerated() {
        let log = parse_log("\u{feff}{\"version\":\"2.1.0\",\"runs\":[]}").unwrap();
        assert!(log.runs.is_empty());
    }
}
