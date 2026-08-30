//! `.http` / `.rest` request files (#370): the REST-Client format — requests
//! separated by `###`, `Name: value` headers, a blank line before the body,
//! `{{variables}}` from a `.http.env.json` beside the file — parsed, sent,
//! and rendered as a response document the editor opens as an ordinary tab.
//!
//! One divergence from REST Client worth knowing: `ureq` replaces duplicate
//! request headers, so two `Accept:` lines in a file send only the last.
//!
//! The split of responsibilities is deliberate: everything here is pure or
//! blocking-IO-free except [`send`], which the app runs on a worker thread.
//! Secrets never leave the substitution: history records the RAW request
//! text (variables unresolved), and the response document carries only the
//! raw request line plus what the server sent back.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// A response body larger than this is cut, and the document says so. The
/// viewer is an editor tab, not a download manager.
pub const RESPONSE_CAP_BYTES: usize = 8 * 1024 * 1024;

/// How long a request may take before the worker gives up.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The env file read beside a request file: a flat JSON object whose values
/// fill `{{name}}` holes.
pub const ENV_FILE: &str = ".http.env.json";

/// One request block in a `.http` file, with the rows it spans so the app
/// can find the request under the caret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    /// First row of the block (its `###` separator or first content line).
    pub start: usize,
    /// Row of the request line itself.
    pub line: usize,
    /// One past the last row of the block.
    pub end: usize,
}

/// Whether `path` is a request file this module owns.
pub fn is_http_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("http") | Some("rest")
    )
}

/// Parse every request block in `text`. Blocks are separated by lines
/// starting with `###`; within a block, `#` and `//` lines are comments,
/// the first remaining line is `[METHOD] URL [HTTP/version]`, headers follow
/// until a blank line, and everything after is the body.
pub fn parse_requests(text: &str) -> Vec<HttpRequest> {
    // A leading BOM (routine from Windows editors) must not defeat the
    // method test on the first request line.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    let mut start = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("###") {
            if i > start {
                blocks.push((start, i));
            }
            start = i;
        }
    }
    if lines.len() > start {
        blocks.push((start, lines.len()));
    }
    let mut out = Vec::new();
    for (s, e) in blocks {
        if let Some(req) = parse_block(&lines, s, e) {
            out.push(req);
        }
    }
    out
}

fn parse_block(lines: &[&str], start: usize, end: usize) -> Option<HttpRequest> {
    let mut i = start;
    // Skip the separator itself, blanks, and comments to the request line.
    while i < end {
        let t = lines[i].trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with("//") {
            i += 1;
            continue;
        }
        break;
    }
    if i >= end {
        return None;
    }
    let line_row = i;
    let mut parts = lines[i].split_whitespace();
    let first = parts.next()?;
    // `GET https://…` or a bare URL (GET implied). A method is an all-caps
    // token WITH a URL after it; a lone all-caps token is treated as the
    // URL, so a malformed line yields an honest send error rather than
    // silently dropping the whole block from `request_at`.
    let looks_like_method =
        first.len() <= 10 && !first.is_empty() && first.chars().all(|c| c.is_ascii_uppercase());
    let (method, url) = match parts.next() {
        Some(second) if looks_like_method => (first.to_string(), second.to_string()),
        _ => (String::from("GET"), first.to_string()),
    };
    i += 1;
    // Headers until the first blank line.
    let mut headers = Vec::new();
    while i < end {
        let t = lines[i].trim();
        if t.is_empty() {
            break;
        }
        if t.starts_with('#') || t.starts_with("//") {
            i += 1;
            continue;
        }
        if let Some((name, value)) = t.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
        i += 1;
    }
    // Body: the rest of the block, outer blank lines trimmed.
    let mut body_lines: Vec<&str> = lines[i.min(end)..end].to_vec();
    while body_lines.first().is_some_and(|l| l.trim().is_empty()) {
        body_lines.remove(0);
    }
    while body_lines.last().is_some_and(|l| l.trim().is_empty()) {
        body_lines.pop();
    }
    let body = if body_lines.is_empty() {
        None
    } else {
        Some(body_lines.join("\n"))
    };
    Some(HttpRequest {
        method,
        url,
        headers,
        body,
        start,
        line: line_row,
        end,
    })
}

/// The request whose block contains `row`, for "run the request under the
/// caret".
pub fn request_at(requests: &[HttpRequest], row: usize) -> Option<&HttpRequest> {
    requests.iter().find(|r| r.start <= row && row < r.end)
}

/// Read the flat `{{name}} -> value` map from [`ENV_FILE`] in `dir`. String
/// values verbatim; numbers and booleans stringified; anything nested is
/// skipped (this is a variable file, not a config format).
pub fn load_env(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(bytes) = std::fs::read(dir.join(ENV_FILE)) else {
        return out;
    };
    let Ok(serde_json::Value::Object(map)) = serde_json::from_slice(&bytes) else {
        return out;
    };
    for (k, v) in map {
        let val = match v {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            _ => continue,
        };
        out.insert(k, val);
    }
    out
}

/// Fill every `{{name}}` hole from `vars` (or the process environment for
/// `{{$env.NAME}}`), returning the result and the names that had no value.
/// An unknown hole is left verbatim so the caller can refuse to send a
/// request whose secrets or hosts would otherwise go out literally.
pub fn substitute(text: &str, vars: &BTreeMap<String, String>) -> (String, Vec<String>) {
    let mut out = String::with_capacity(text.len());
    let mut missing = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let name = after[..close].trim();
        let value = if let Some(env_name) = name.strip_prefix("$env.") {
            std::env::var(env_name).ok()
        } else {
            vars.get(name).cloned()
        };
        match value {
            Some(v) => out.push_str(&v),
            None => {
                out.push_str(&rest[open..open + 2 + close + 2]);
                if !missing.contains(&name.to_string()) {
                    missing.push(name.to_string());
                }
            }
        }
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    (out, missing)
}

/// A request with its variables already substituted, ready to send.
#[derive(Clone, Debug)]
pub struct ResolvedRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// Substitute every hole in `req` from `vars`; the missing names cover the
/// method line, headers, and body together.
pub fn resolve(
    req: &HttpRequest,
    vars: &BTreeMap<String, String>,
) -> (ResolvedRequest, Vec<String>) {
    let mut missing: Vec<String> = Vec::new();
    let mut sub = |s: &str| {
        let (v, m) = substitute(s, vars);
        for name in m {
            // `Vec::dedup` removes only CONSECUTIVE duplicates; a name
            // missing in both the URL and a later header must still be
            // reported once.
            if !missing.contains(&name) {
                missing.push(name);
            }
        }
        v
    };
    let resolved = ResolvedRequest {
        method: req.method.clone(),
        url: sub(&req.url),
        headers: req
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), sub(v)))
            .collect(),
        body: req.body.as_deref().map(&mut sub),
    };
    (resolved, missing)
}

/// The request as a `curl` command line, single-quoted for a POSIX shell.
pub fn to_curl(req: &ResolvedRequest) -> String {
    fn sq(s: &str) -> String {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
    let mut out = format!("curl -X {} {}", req.method, sq(&req.url));
    for (k, v) in &req.headers {
        out.push_str(&format!(" -H {}", sq(&format!("{k}: {v}"))));
    }
    if let Some(body) = &req.body {
        out.push_str(&format!(" --data {}", sq(body)));
    }
    out
}

/// What came back: enough to render the response document, never more than
/// [`RESPONSE_CAP_BYTES`] of body.
#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub http_version: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub truncated: bool,
    pub elapsed_ms: u64,
}

impl HttpResponse {
    /// The `content-type` header's value, lowercased, parameters dropped.
    pub fn content_type(&self) -> String {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| {
                v.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase()
            })
            .unwrap_or_default()
    }
}

/// Send `req` and read the response, on the caller's thread (the app runs
/// this on a worker). A non-2xx status is a RESPONSE, not an error; only a
/// transport failure (refused, DNS, TLS, timeout) is `Err`.
pub fn send(req: &ResolvedRequest, timeout: Duration) -> Result<HttpResponse, String> {
    use std::io::Read;
    let agent = ureq::AgentBuilder::new()
        .timeout(timeout)
        .redirects(4)
        .build();
    let mut r = agent.request(&req.method, &req.url);
    for (k, v) in &req.headers {
        r = r.set(k, v);
    }
    let started = std::time::Instant::now();
    let result = match &req.body {
        Some(body) => r.send_string(body),
        None => r.call(),
    };
    let resp = match result {
        Ok(resp) => resp,
        // A status error still carries the whole response; show it.
        Err(ureq::Error::Status(_, resp)) => resp,
        // The KIND only, never the Display: ureq's transport Display opens
        // with the resolved URL, and a `?api_key={{token}}` request would
        // print the substituted secret into the status line on exactly the
        // failures (refused, DNS, timeout) a user reads it for.
        Err(ureq::Error::Transport(t)) => return Err(t.kind().to_string()),
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let status = resp.status();
    let status_text = resp.status_text().to_string();
    let http_version = resp.http_version().to_string();
    let headers: Vec<(String, String)> = resp
        .headers_names()
        .into_iter()
        .map(|n| {
            let v = resp.header(&n).unwrap_or("").to_string();
            (n, v)
        })
        .collect();
    let mut body = Vec::new();
    let mut reader = resp.into_reader().take(RESPONSE_CAP_BYTES as u64 + 1);
    reader
        .read_to_end(&mut body)
        .map_err(|e| format!("reading the response body: {e}"))?;
    let truncated = body.len() > RESPONSE_CAP_BYTES;
    body.truncate(RESPONSE_CAP_BYTES);
    Ok(HttpResponse {
        status,
        status_text,
        http_version,
        headers,
        body,
        truncated,
        elapsed_ms,
    })
}

/// How the response body should be materialised for the viewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResponseKind {
    /// A text document with the given extension (`jsonc`, `xml`, `txt`):
    /// status and headers ride along as comments, the body pretty-printed.
    Text(&'static str),
    /// An image: the raw bytes are written with this extension and the
    /// editor's image path renders them.
    Image(&'static str),
}

/// Classify `content_type` into how the response opens.
pub fn response_kind(content_type: &str) -> ResponseKind {
    match content_type {
        "image/png" => ResponseKind::Image("png"),
        "image/jpeg" | "image/jpg" => ResponseKind::Image("jpg"),
        "image/gif" => ResponseKind::Image("gif"),
        "image/webp" => ResponseKind::Image("webp"),
        "image/bmp" => ResponseKind::Image("bmp"),
        t if t == "application/json" || t.ends_with("+json") => ResponseKind::Text("jsonc"),
        t if t == "application/xml" || t == "text/xml" || t.ends_with("+xml") => {
            ResponseKind::Text("xml")
        }
        _ => ResponseKind::Text("txt"),
    }
}

/// Render a text response as the document the tab shows: the RAW request
/// line (variables unresolved, so a secret never lands in the file), the
/// status, timing and headers as comments in the target language, then the
/// body — pretty-printed for JSON, verbatim otherwise. Returns the file
/// extension and the text.
pub fn response_doc(raw_request_line: &str, resp: &HttpResponse) -> (&'static str, String) {
    let kind = response_kind(&resp.content_type());
    let ext = match kind {
        ResponseKind::Text(e) => e,
        // The caller routes images before coming here; falling through
        // renders the metadata alone rather than binary garbage.
        ResponseKind::Image(_) => "txt",
    };
    let (open, close): (&str, &str) = match ext {
        "xml" => ("<!-- ", " -->"),
        _ => ("// ", ""),
    };
    // A server header containing `-->` must not escape the XML comment
    // prologue into the document body: the tab is a viewer of untrusted
    // server output. (`//` comments are newline-delimited and ureq strips
    // newlines from header values, so the other branch needs no escape.)
    let esc = |s: &str| -> String {
        if ext == "xml" {
            s.replace("-->", "--&gt;")
        } else {
            s.to_string()
        }
    };
    let mut doc = String::new();
    let size = resp.body.len();
    doc.push_str(&format!(
        "{open}{} \u{2192} HTTP/{} {} {} \u{00b7} {} ms \u{00b7} {size} B{close}\n",
        esc(raw_request_line),
        resp.http_version.trim_start_matches("HTTP/"),
        resp.status,
        esc(&resp.status_text),
        resp.elapsed_ms,
    ));
    for (k, v) in &resp.headers {
        doc.push_str(&format!("{open}{}: {}{close}\n", esc(k), esc(v)));
    }
    if resp.truncated {
        doc.push_str(&format!(
            "{open}body cut at {RESPONSE_CAP_BYTES} bytes{close}\n"
        ));
    }
    doc.push('\n');
    let body_text = String::from_utf8_lossy(&resp.body);
    if ext == "jsonc" {
        match serde_json::from_str::<serde_json::Value>(&body_text) {
            Ok(v) => {
                doc.push_str(
                    &serde_json::to_string_pretty(&v).unwrap_or_else(|_| body_text.to_string()),
                );
            }
            Err(_) => doc.push_str(&body_text),
        }
    } else {
        doc.push_str(&body_text);
    }
    if !doc.ends_with('\n') {
        doc.push('\n');
    }
    (ext, doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    const THREE: &str = "\
# users, with a token from the env file
GET {{host}}/users\n\
Authorization: Bearer {{token}}\n\
\n\
###\n\
POST {{host}}/users HTTP/1.1\n\
Content-Type: application/json\n\
\n\
{\"name\": \"ada\"}\n\
\n\
### delete one\n\
DELETE {{host}}/users/1\n";

    /// The issue's shape: three requests split on `###`, each independent,
    /// with comments and a method-less line handled.
    #[test]
    fn parses_three_requests_with_separators_headers_and_bodies() {
        let reqs = parse_requests(THREE);
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[0].method, "GET");
        assert_eq!(reqs[0].url, "{{host}}/users");
        assert_eq!(
            reqs[0].headers,
            vec![(
                String::from("Authorization"),
                String::from("Bearer {{token}}")
            )]
        );
        assert_eq!(reqs[0].body, None);
        assert_eq!(reqs[1].method, "POST");
        assert_eq!(reqs[1].body.as_deref(), Some("{\"name\": \"ada\"}"));
        assert_eq!(reqs[2].method, "DELETE");
        assert_eq!(reqs[2].line, 11, "the request line, not the ### above it");

        // A bare URL is a GET.
        let bare = parse_requests("example.com/health\n");
        assert_eq!(bare[0].method, "GET");
        assert_eq!(bare[0].url, "example.com/health");
    }

    /// The caret maps to the block it sits in, separators included, so the
    /// chord works from anywhere inside a request.
    #[test]
    fn request_at_covers_each_whole_block() {
        let reqs = parse_requests(THREE);
        assert_eq!(request_at(&reqs, 0).unwrap().method, "GET");
        assert_eq!(request_at(&reqs, 3).unwrap().method, "GET");
        assert_eq!(
            request_at(&reqs, 4).unwrap().method,
            "POST",
            "the ### row belongs to the next block"
        );
        assert_eq!(request_at(&reqs, 8).unwrap().method, "POST");
        assert_eq!(request_at(&reqs, 11).unwrap().method, "DELETE");
        assert!(request_at(&reqs, 99).is_none());
    }

    /// Variables fill from the map and `$env.`; a hole with no value is
    /// left verbatim and NAMED, so the caller can refuse to send it.
    #[test]
    fn substitution_fills_names_env_and_reports_missing() {
        let mut vars = BTreeMap::new();
        vars.insert(String::from("host"), String::from("http://x.test"));
        let (s, missing) = substitute("{{host}}/u/{{id}}", &vars);
        assert_eq!(s, "http://x.test/u/{{id}}");
        assert_eq!(missing, vec![String::from("id")]);

        // SAFETY: the var name is unique to this test and nothing else in
        // the tree reads it, so no concurrent reader races the mutation.
        unsafe { std::env::set_var("CROFT_HTTP_TEST_VAR", "7") };
        let (s, missing) = substitute("v={{$env.CROFT_HTTP_TEST_VAR}}", &vars);
        assert_eq!(s, "v=7");
        assert!(missing.is_empty());

        let (reqs, vars2) = (parse_requests(THREE), {
            let mut v = vars.clone();
            v.insert(String::from("token"), String::from("s3cret"));
            v
        });
        let (resolved, missing) = resolve(&reqs[0], &vars2);
        assert!(missing.is_empty());
        assert_eq!(resolved.url, "http://x.test/users");
        assert_eq!(resolved.headers[0].1, "Bearer s3cret");
    }

    /// A malformed request line and a BOM'd file degrade honestly rather
    /// than silently dropping a block (#428 review).
    #[test]
    fn malformed_lines_and_boms_degrade_honestly() {
        // A lone all-caps token is a URL, not a method with no URL: the
        // block survives, and the send fails with a real error later.
        let reqs = parse_requests("GET\nAccept: x\n\n###\nGET http://ok/\n");
        assert_eq!(reqs.len(), 2, "the malformed block is not dropped");
        assert_eq!(reqs[0].url, "GET");
        assert_eq!(request_at(&reqs, 1).unwrap().url, "GET");

        // A leading BOM does not become part of the first URL.
        let reqs = parse_requests("\u{feff}GET http://x/a\n");
        assert_eq!(reqs[0].method, "GET");
        assert_eq!(reqs[0].url, "http://x/a");
    }

    /// A name missing in two places is reported once, even with another
    /// missing name between the occurrences (`dedup` is consecutive-only).
    #[test]
    fn a_missing_name_is_reported_once_across_the_request() {
        let req = HttpRequest {
            method: String::from("GET"),
            url: String::from("{{tok}}/{{host}}"),
            headers: vec![(String::from("X-A"), String::from("{{tok}}"))],
            body: None,
            start: 0,
            line: 0,
            end: 1,
        };
        let (_, missing) = resolve(&req, &BTreeMap::new());
        assert_eq!(missing, vec![String::from("tok"), String::from("host")]);
    }

    /// A server header carrying `-->` cannot escape the XML comment
    /// prologue into the rendered document (#428 review).
    #[test]
    fn xml_prologue_survives_a_hostile_header_value() {
        let resp = HttpResponse {
            status: 200,
            status_text: String::from("OK"),
            http_version: String::from("HTTP/1.1"),
            headers: vec![
                (String::from("content-type"), String::from("text/xml")),
                (String::from("x-evil"), String::from("a --> <boom/> <!--")),
            ],
            body: b"<a/>".to_vec(),
            truncated: false,
            elapsed_ms: 1,
        };
        let (ext, doc) = response_doc("GET x", &resp);
        assert_eq!(ext, "xml");
        assert!(
            doc.contains("a --&gt; <boom/> <!-- -->"),
            "the header stays inside its comment: {doc}"
        );
        assert!(!doc.contains("a --> <boom/>"), "no raw escape survives");
    }

    /// The env file is a flat JSON object; strings verbatim, numbers and
    /// booleans stringified, nesting skipped, absence empty.
    #[test]
    fn env_file_loads_flat_values_only() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join(ENV_FILE),
            "{\"host\": \"http://a\", \"port\": 8080, \"on\": true, \"nested\": {\"x\": 1}}",
        )
        .unwrap();
        let vars = load_env(dir.path());
        assert_eq!(vars.get("host").map(String::as_str), Some("http://a"));
        assert_eq!(vars.get("port").map(String::as_str), Some("8080"));
        assert_eq!(vars.get("on").map(String::as_str), Some("true"));
        assert!(!vars.contains_key("nested"));
        assert!(load_env(&dir.path().join("missing")).is_empty());
    }

    /// Copy as curl single-quotes for a POSIX shell, apostrophes included.
    #[test]
    fn curl_line_quotes_for_a_posix_shell() {
        let req = ResolvedRequest {
            method: String::from("POST"),
            url: String::from("http://x.test/it's"),
            headers: vec![(String::from("X-A"), String::from("b c"))],
            body: Some(String::from("{\"q\": \"a'b\"}")),
        };
        let curl = to_curl(&req);
        assert_eq!(
            curl,
            "curl -X POST 'http://x.test/it'\\''s' -H 'X-A: b c' --data '{\"q\": \"a'\\''b\"}'"
        );
    }

    /// A JSON response renders as a `.jsonc` document: the RAW request line
    /// and every header as `//` comments, the body pretty-printed; XML gets
    /// XML comments; anything else is a `.txt` with the body verbatim.
    #[test]
    fn response_documents_render_by_content_type() {
        let mut resp = HttpResponse {
            status: 200,
            status_text: String::from("OK"),
            http_version: String::from("HTTP/1.1"),
            headers: vec![(
                String::from("content-type"),
                String::from("application/json; charset=utf-8"),
            )],
            body: b"{\"a\":[1,2]}".to_vec(),
            truncated: false,
            elapsed_ms: 42,
        };
        let (ext, doc) = response_doc("GET {{host}}/users", &resp);
        assert_eq!(ext, "jsonc");
        assert!(doc.starts_with(
            "// GET {{host}}/users \u{2192} HTTP/1.1 200 OK \u{00b7} 42 ms \u{00b7} 11 B\n"
        ));
        assert!(doc.contains("// content-type: application/json"));
        assert!(
            doc.contains("{\n  \"a\": [\n    1,\n    2\n  ]\n}"),
            "pretty-printed: {doc}"
        );

        resp.headers[0].1 = String::from("text/xml");
        resp.body = b"<a/>".to_vec();
        let (ext, doc) = response_doc("GET x", &resp);
        assert_eq!(ext, "xml");
        assert!(doc.contains("<!-- GET x"));
        assert!(doc.ends_with("<a/>\n"));

        resp.headers.clear();
        resp.truncated = true;
        let (ext, doc) = response_doc("GET x", &resp);
        assert_eq!(ext, "txt");
        assert!(doc.contains("// body cut at"));

        assert_eq!(response_kind("image/png"), ResponseKind::Image("png"));
        assert_eq!(
            response_kind("application/hal+json"),
            ResponseKind::Text("jsonc")
        );
    }
}
