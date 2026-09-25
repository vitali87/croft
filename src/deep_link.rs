//! `croft://` links (#359): tap a notification, land in the session.
//!
//! `croft://attach?host=<ssh alias>&path=<workspace>&focus=<terminal|editor>`
//! opens `croft remote <host> <path>`, or `croft attach <path>` without a
//! host. Any web page can hand the OS a link, so a link can only do what the
//! user could already do by name: the host must be an alias in their
//! `~/.ssh/config`, and nothing may start with `-` (it would read as a flag).
//! `croft install-link-handler` registers the scheme with the desktop
//! (xdg) or with Termux's URL opener; the macOS launcher registers it itself.

use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Terminal,
    Editor,
}

impl Focus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Editor => "editor",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub host: Option<String>,
    pub path: Option<String>,
    pub focus: Option<Focus>,
}

/// Decode `%XX` and `+` in a query value.
fn percent_decode(s: &str) -> Result<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = s
                    .get(i + 1..i + 3)
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                let Some(b) = hex else {
                    bail!("bad escape in link");
                };
                out.push(b);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| anyhow::anyhow!("link is not UTF-8"))
}

fn safe_host(h: &str) -> bool {
    !h.is_empty()
        && !h.starts_with('-')
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-@".contains(c))
}

/// Parse and check a `croft://attach?…` link.
pub fn parse(url: &str) -> Result<Link> {
    let Some(rest) = url.strip_prefix("croft://") else {
        bail!("not a croft:// link");
    };
    let (action, query) = rest.split_once('?').unwrap_or((rest, ""));
    if action.trim_end_matches('/') != "attach" {
        bail!("croft:// links support only `attach`");
    }
    let mut link = Link {
        host: None,
        path: None,
        focus: None,
    };
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = percent_decode(v)?;
        match k {
            "host" => {
                if !safe_host(&v) {
                    bail!("refusing host {v:?}");
                }
                link.host = Some(v);
            }
            "path" => {
                if v.is_empty() || v.starts_with('-') || v.contains('\0') {
                    bail!("refusing path {v:?}");
                }
                link.path = Some(v);
            }
            "focus" => {
                link.focus = Some(match v.as_str() {
                    "terminal" | "pane" => Focus::Terminal,
                    "editor" => Focus::Editor,
                    other => bail!("unknown focus {other:?}"),
                });
            }
            // Unknown keys are ignored, so a link from a newer croft still
            // opens the session in an older one.
            _ => {}
        }
    }
    Ok(link)
}

/// Which ssh alias a link's host means: an alias itself, or the alias
/// whose `HostName` it is (a notification names the machine, the phone's
/// ssh config names an alias for it). This machine's own name means a
/// local session (`Ok(None)`); anything else is refused.
pub fn resolve_host(
    host: &str,
    local_hostname: &str,
    targets: &[(String, Option<String>)],
) -> Result<Option<String>> {
    if let Some((alias, _)) = targets.iter().find(|(a, _)| a == host) {
        return Ok(Some(alias.clone()));
    }
    if let Some((alias, _)) = targets.iter().find(|(_, name)| {
        name.as_deref()
            .is_some_and(|n| n.eq_ignore_ascii_case(host))
    }) {
        return Ok(Some(alias.clone()));
    }
    if host.eq_ignore_ascii_case(local_hostname) {
        return Ok(None);
    }
    bail!("{host} is not a host in your ~/.ssh/config, so the link was not opened")
}

/// The croft arguments that carry out `link`, once its host has been
/// checked against the ssh config.
pub fn argv(link: &Link) -> Vec<String> {
    let mut out = Vec::new();
    match &link.host {
        Some(h) => {
            out.push(String::from("remote"));
            out.push(h.clone());
        }
        None => out.push(String::from("attach")),
    }
    if let Some(p) = &link.path {
        out.push(p.clone());
    }
    out
}

/// The xdg desktop entry that routes `croft://` to `croft open-link`.
pub fn desktop_entry(croft_bin: &str) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Croft link\nNoDisplay=true\nTerminal=true\nExec=\"{croft_bin}\" open-link %u\nMimeType=x-scheme-handler/croft;\n"
    )
}

/// Termux's `~/bin/termux-url-opener`: `croft://` links open croft, and
/// anything else keeps Termux's usual handling.
pub fn termux_url_opener(croft_bin: &str) -> String {
    format!(
        "#!/data/data/com.termux/files/usr/bin/sh\n# written by croft install-link-handler\ncase \"$1\" in\n  croft://*) exec \"{croft_bin}\" open-link \"$1\" ;;\n  *) exec termux-open-url \"$1\" ;;\nesac\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_link_becomes_croft_remote() {
        let l = parse("croft://attach?host=devbox&path=%2Fsrv%2Fapp&focus=terminal").unwrap();
        assert_eq!(l.host.as_deref(), Some("devbox"));
        assert_eq!(l.path.as_deref(), Some("/srv/app"));
        assert_eq!(l.focus, Some(Focus::Terminal));
        assert_eq!(argv(&l), vec!["remote", "devbox", "/srv/app"]);
    }

    #[test]
    fn a_link_without_a_host_attaches_locally() {
        let l = parse("croft://attach?path=~/my+proj").unwrap();
        assert_eq!(argv(&l), vec!["attach", "~/my proj"]);
        assert_eq!(argv(&parse("croft://attach").unwrap()), vec!["attach"]);
    }

    #[test]
    fn links_that_could_smuggle_flags_or_commands_are_refused() {
        for bad in [
            "croft://attach?host=-oProxyCommand=evil",
            "croft://attach?host=a%20b",
            "croft://attach?host=a;rm",
            "croft://attach?path=--solo",
            "croft://attach?path=%00",
            "croft://run?cmd=ls",
            "https://attach?host=a",
            "croft://attach?focus=shell",
            "croft://attach?path=%zz",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_host_resolves_by_alias_by_hostname_or_as_this_machine() {
        let targets = vec![
            (
                String::from("devbox"),
                Some(String::from("dev.example.com")),
            ),
            (String::from("pi"), None),
        ];
        assert_eq!(
            resolve_host("pi", "me", &targets).unwrap().as_deref(),
            Some("pi")
        );
        assert_eq!(
            resolve_host("DEV.example.com", "me", &targets)
                .unwrap()
                .as_deref(),
            Some("devbox")
        );
        assert_eq!(resolve_host("me", "me", &targets).unwrap(), None);
        assert!(resolve_host("elsewhere", "me", &targets).is_err());
    }

    #[test]
    fn handlers_route_the_scheme_to_open_link() {
        assert!(desktop_entry("/b/croft").contains("MimeType=x-scheme-handler/croft;"));
        assert!(desktop_entry("/b/croft").contains("Exec=\"/b/croft\" open-link %u"));
        let t = termux_url_opener("/b/croft");
        assert!(t.contains("croft://*) exec \"/b/croft\" open-link \"$1\""));
        assert!(t.contains("termux-open-url"));
    }
}
