//! `croft pair`'s provider flags as recorded: `--base-url` alone means ollama, and ollama
//! without an endpoint gets the standard local port.

use super::*;

/// Resolve the pair provider flags as recorded: `--base-url` alone implies
/// ollama, and ollama without an explicit endpoint gets the standard local
/// port. A plain claude activation records neither.
pub(super) fn resolve_pair_provider(
    provider: Option<String>,
    base_url: Option<String>,
) -> Result<(Option<String>, Option<String>)> {
    let provider = provider.or_else(|| base_url.is_some().then(|| String::from("ollama")));
    // Validate at activation time, when the user is watching: a malformed
    // endpoint otherwise surfaces hours later as an opaque failure on the
    // first turn. A trailing slash would build `...//v1/messages`, which
    // routers 301 and ureq follows by downgrading POST to GET; a prefix
    // check alone admitted empty hosts (`https:///path`), whitespace, and
    // malformed ports, so the URL is properly parsed.
    let base_url = base_url
        .map(|u| {
            let u = u.trim().trim_end_matches('/').to_string();
            let parsed = url::Url::parse(&u)
                .map_err(|e| anyhow::anyhow!("--base-url is not a valid URL ({e}): {u}"))?;
            anyhow::ensure!(
                matches!(parsed.scheme(), "http" | "https"),
                "--base-url must use http or https (got {u})"
            );
            anyhow::ensure!(
                parsed.host_str().is_some_and(|h| !h.is_empty()),
                "--base-url needs a host (got {u})"
            );
            Ok(u)
        })
        .transpose()?;
    let base_url = match provider.as_deref() {
        Some("ollama") => base_url.or_else(|| Some(String::from("http://localhost:11434"))),
        _ => base_url,
    };
    Ok((provider, base_url))
}
