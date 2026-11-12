//! `croft://` links (#359): tap a notification, land in the session.
//!
//! `croft://attach?host=<ssh alias>&path=<workspace>&focus=<terminal|editor|approval>`
//! opens `croft remote <host> <path>`, or `croft attach <path>` without a
//! host. `croft://decide?host=…&path=…&token=…&decision=<allow|deny>`
//! answers an agent's pending edit without attaching: it runs `croft
//! decide` there, over ssh for a remote host. The token is the one-time
//! secret the approval notification carried, so a link some page made up
//! answers nothing. Any web page can hand the OS a link, so a link can only do what the
//! user could already do by name: the host must be an alias in their
//! `~/.ssh/config`, and nothing may start with `-` (it would read as a flag).
//! `croft install-link-handler` registers the scheme with the desktop
//! (xdg) or with Termux's URL opener; the macOS launcher registers it itself.
//!
//! The launcher registers `vscode:` too, so Croft can stand in for VS Code:
//! [`route`] opens a `vscode://file/<path>:<line>` link's file here and hands
//! every other `vscode:` link to VS Code unchanged.

use anyhow::{Result, bail};

use crate::file_ref::FileRef;

/// What `croft open-link` does with a link the desktop handed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// A `croft://` link: attach to, or answer, the session it names.
    Croft(Link),
    /// A `vscode://file/<path>[:line[:col]]` link: open that file here.
    Open(FileRef),
    /// Any other `vscode:` link (an extension install, a git clone): VS
    /// Code's own business, handed to it unchanged.
    Vscode(String),
}

/// VS Code's bundle id, which the links it keeps are handed to by name.
const VSCODE_BUNDLE_ID: &str = "com.microsoft.VSCode";

/// Sort a link the desktop handed croft: a `croft://` link is checked by
/// [`parse`], a `vscode://file/…` link opens its file here, and any other
/// `vscode:` link is left to VS Code.
pub fn route(url: &str) -> Result<Route> {
    let Some((scheme, rest)) = url.split_once(':') else {
        bail!("{url:?} is not a link");
    };
    if scheme.eq_ignore_ascii_case("croft") {
        return parse(url).map(Route::Croft);
    }
    // The launcher registers only these two schemes; anything else reaching
    // here was typed, and croft has nothing to do with it.
    if !scheme.eq_ignore_ascii_case("vscode") {
        bail!("croft opens croft:// and vscode:// links, not {scheme}:");
    }
    let authority = rest
        .strip_prefix("//")
        .and_then(|r| r.split(['/', '?', '#']).next());
    if authority != Some("file") {
        return Ok(Route::Vscode(url.to_string()));
    }
    match crate::file_ref::editor_file_uri(url) {
        Some(file) => Ok(Route::Open(file)),
        None => bail!("{url} names no file"),
    }
}

/// Whether carrying out `route` needs a croft window. A link VS Code keeps
/// does not: it is handed over without opening one.
pub fn needs_terminal(route: &Route) -> bool {
    !matches!(route, Route::Vscode(_))
}

/// The croft argument that opens a file link's file at its position, in the
/// `path:line[:col]` form croft takes from a shell. A link with no position
/// is the bare path, so a link to a folder opens the folder.
pub fn open_file_arg(file: &FileRef) -> String {
    match (file.line, file.column) {
        (1, None) => file.path.clone(),
        (line, None) => format!("{}:{line}", file.path),
        (line, Some(column)) => format!("{}:{line}:{column}", file.path),
    }
}

/// Hand `url` to VS Code through `opener` (`/usr/bin/open` in use). `-b`
/// names VS Code's bundle rather than the scheme: once Croft is the
/// scheme's default handler, `open <url>` would hand the link straight back.
pub fn forward_to_vscode(url: &str, mut opener: std::process::Command) -> Result<()> {
    let out = opener
        .args(["-b", VSCODE_BUNDLE_ID, url])
        .output()
        .map_err(|e| anyhow::anyhow!("could not hand {url} to VS Code: {e}"))?;
    if !out.status.success() {
        // `open` says why, e.g. VS Code is not installed.
        bail!(
            "VS Code did not take {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Terminal,
    Editor,
    /// The agent edit waiting for approval (#359).
    Approval,
}

impl Focus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Editor => "editor",
            Self::Approval => "approval",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub host: Option<String>,
    pub path: Option<String>,
    pub focus: Option<Focus>,
    /// Set for a `croft://decide` link: answer, don't attach.
    pub decide: Option<Decide>,
}

/// A `croft://decide` link's answer to a pending agent edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decide {
    pub token: String,
    pub allow: bool,
}

/// A token as croft issues them: 32 lowercase hex digits. Anything else
/// is refused before it reaches a shell or a socket.
fn valid_token(t: &str) -> bool {
    t.len() == 32 && t.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
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

/// Parse and check a `croft://attach?…` or `croft://decide?…` link.
pub fn parse(url: &str) -> Result<Link> {
    let Some(rest) = url.strip_prefix("croft://") else {
        bail!("not a croft:// link");
    };
    let (action, query) = rest.split_once('?').unwrap_or((rest, ""));
    let deciding = match action.trim_end_matches('/') {
        "attach" => false,
        "decide" => true,
        _ => bail!("croft:// links support only `attach` and `decide`"),
    };
    let mut link = Link {
        host: None,
        path: None,
        focus: None,
        decide: None,
    };
    let (mut token, mut allow) = (None, None);
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
                    "approval" => Focus::Approval,
                    other => bail!("unknown focus {other:?}"),
                });
            }
            "token" if deciding => {
                if !valid_token(&v) {
                    bail!("refusing token");
                }
                token = Some(v);
            }
            "decision" if deciding => {
                allow = Some(match v.as_str() {
                    "allow" => true,
                    "deny" => false,
                    other => bail!("unknown decision {other:?}"),
                });
            }
            // Unknown keys are ignored, so a link from a newer croft still
            // opens the session in an older one.
            _ => {}
        }
    }
    if deciding {
        let (Some(token), Some(allow)) = (token, allow) else {
            bail!("a decide link needs a token and a decision");
        };
        // Which croft to answer is found from the workspace, so a decide
        // link without one could only guess.
        if link.path.is_none() {
            bail!("a decide link needs the workspace path");
        }
        link.decide = Some(Decide { token, allow });
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
/// checked against the ssh config. `home` expands a local `~`: no shell
/// reads these arguments, so `~/proj` would otherwise name a directory
/// called `~`.
pub fn argv(link: &Link, home: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    match &link.host {
        Some(h) => {
            out.push(String::from("remote"));
            out.push(h.clone());
        }
        None => out.push(String::from("attach")),
    }
    out.extend(path_arg(link, home));
    out
}

/// The `croft decide` arguments a decide link stands for, to run here or,
/// for a link with a host, on that host (see [`remote_shell_command`]).
pub fn decide_argv(link: &Link, home: Option<&str>) -> Option<Vec<String>> {
    let d = link.decide.as_ref()?;
    let mut out = vec![
        String::from("decide"),
        d.token.clone(),
        String::from(if d.allow { "allow" } else { "deny" }),
    ];
    if let Some(p) = path_arg(link, home) {
        out.push(String::from("--path"));
        out.push(p);
    }
    Some(out)
}

/// `croft <args>` as the one command string ssh hands the remote login
/// shell, every argument single-quoted. The PATH line matches `croft
/// remote`'s, so a croft installed by cargo is found.
pub fn remote_shell_command(args: &[String]) -> String {
    let mut s = String::from("export PATH=\"$HOME/.cargo/bin:$PATH\"; croft");
    for a in args {
        s.push_str(" '");
        s.push_str(&a.replace('\'', "'\\''"));
        s.push('\'');
    }
    s
}

/// The link's path as croft's argument: `~` expanded locally, relative to
/// the login directory remotely.
fn path_arg(link: &Link, home: Option<&str>) -> Option<String> {
    let p = link.path.as_ref()?;
    Some(match (&link.host, p.strip_prefix('~')) {
        // The remote croft quotes its path, so `~` is never expanded
        // there either; the login shell starts in home, so a path
        // relative to it is the same directory.
        (Some(_), Some("")) => String::from("."),
        // `./` keeps `~/-x` from reading as a flag once the `~` is gone.
        (Some(_), Some(rest)) if rest.starts_with('/') => format!(".{rest}"),
        (None, Some(rest)) if rest.is_empty() || rest.starts_with('/') => match home {
            Some(h) => format!("{}{rest}", h.trim_end_matches('/')),
            None => p.clone(),
        },
        _ => p.clone(),
    })
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
        assert_eq!(argv(&l, None), vec!["remote", "devbox", "/srv/app"]);
    }

    #[test]
    fn a_link_without_a_host_attaches_locally() {
        let l = parse("croft://attach?path=~/my+proj").unwrap();
        assert_eq!(argv(&l, None), vec!["attach", "~/my proj"]);
        assert_eq!(
            argv(&parse("croft://attach").unwrap(), None),
            vec!["attach"]
        );
    }

    #[test]
    fn a_tilde_path_means_home_locally_and_remotely() {
        let l = parse("croft://attach?path=~/my+proj").unwrap();
        assert_eq!(
            argv(&l, Some("/home/ada/")),
            vec!["attach", "/home/ada/my proj"]
        );
        let l = parse("croft://attach?path=~").unwrap();
        assert_eq!(argv(&l, Some("/home/ada")), vec!["attach", "/home/ada"]);
        let l = parse("croft://attach?path=~bob/x").unwrap();
        assert_eq!(argv(&l, Some("/home/ada")), vec!["attach", "~bob/x"]);
        let mut l = parse("croft://attach?path=~/src").unwrap();
        l.host = Some(String::from("box"));
        assert_eq!(argv(&l, Some("/home/ada")), vec!["remote", "box", "./src"]);
        l.path = Some(String::from("~"));
        assert_eq!(argv(&l, None), vec!["remote", "box", "."]);
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

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn a_decide_link_answers_here_or_over_ssh_without_attaching() {
        let l = parse(&format!(
            "croft://decide?path=~/proj&token={TOKEN}&decision=allow"
        ))
        .unwrap();
        assert_eq!(
            l.decide,
            Some(Decide {
                token: TOKEN.into(),
                allow: true
            })
        );
        assert_eq!(
            decide_argv(&l, Some("/home/ada")).unwrap(),
            ["decide", TOKEN, "allow", "--path", "/home/ada/proj"]
        );
        let mut remote = parse(&format!(
            "croft://decide?host=box&path=%2Fsrv%2Fit%27s&token={TOKEN}&decision=deny"
        ))
        .unwrap();
        remote.host = Some(String::from("box"));
        let args = decide_argv(&remote, None).unwrap();
        assert_eq!(args, ["decide", TOKEN, "deny", "--path", "/srv/it's"]);
        assert_eq!(
            remote_shell_command(&args),
            format!(
                "export PATH=\"$HOME/.cargo/bin:$PATH\"; croft 'decide' '{TOKEN}' 'deny' '--path' '/srv/it'\\''s'"
            )
        );
        // An attach link is not a decision.
        assert_eq!(decide_argv(&parse("croft://attach").unwrap(), None), None);
        assert_eq!(
            parse("croft://attach?focus=approval").unwrap().focus,
            Some(Focus::Approval)
        );
    }

    #[test]
    fn a_decide_link_without_a_well_formed_token_decision_and_path_is_refused() {
        for bad in [
            String::from("croft://decide?path=/w&decision=allow"),
            format!("croft://decide?path=/w&token={TOKEN}"),
            format!("croft://decide?token={TOKEN}&decision=allow"),
            format!("croft://decide?path=/w&token={TOKEN}&decision=maybe"),
            format!(
                "croft://decide?path=/w&token={}&decision=allow",
                &TOKEN[1..]
            ),
            format!(
                "croft://decide?path=/w&token={}&decision=allow",
                TOKEN.to_uppercase()
            ),
            String::from("croft://decide?path=/w&token=%27%3Brm%20-rf%20~%27&decision=allow"),
        ] {
            assert!(parse(&bad).is_err(), "{bad}");
        }
        // On an attach link, token and decision mean nothing.
        let l = parse(&format!("croft://attach?token={TOKEN}&decision=allow")).unwrap();
        assert_eq!(l.decide, None);
    }

    fn opened(url: &str) -> FileRef {
        match route(url) {
            Ok(Route::Open(f)) => f,
            other => panic!("{url} should open a file here, got {other:?}"),
        }
    }

    #[test]
    fn a_vscode_file_link_opens_that_file_here_at_its_line() {
        let f = opened("vscode://file//Users/me/my%20proj/a.rs:42:7");
        assert_eq!(
            (f.path.as_str(), f.line, f.column),
            ("/Users/me/my proj/a.rs", 42, Some(7))
        );
        assert_eq!(open_file_arg(&f), "/Users/me/my proj/a.rs:42:7");
        assert_eq!(
            open_file_arg(&opened("vscode://file/Users/me/a.rs:42")),
            "/Users/me/a.rs:42"
        );
        // No line: the bare path, so a link to a folder opens the folder.
        assert_eq!(
            open_file_arg(&opened("vscode://file//Users/me/proj")),
            "/Users/me/proj"
        );
        assert!(needs_terminal(&route("vscode://file//a.rs").unwrap()));
    }

    #[test]
    fn a_croft_link_still_routes_to_the_session_it_names() {
        let url = "croft://attach?host=devbox&path=%2Fsrv%2Fapp";
        let r = route(url).unwrap();
        assert_eq!(r, Route::Croft(parse(url).unwrap()));
        assert!(needs_terminal(&r));
    }

    /// Extension installs, git clones and extension callbacks are VS Code's,
    /// not files: they go to VS Code byte for byte and open no window here.
    #[test]
    fn other_vscode_links_go_to_vs_code_unchanged_and_are_never_opened_as_files() {
        for url in [
            "vscode://vscode.git/clone?url=https%3A%2F%2Fgithub.com%2Fa%2Fb.git",
            "vscode:extension/rust-lang.rust-analyzer",
            "vscode://ms-vscode-remote.remote-ssh/open?host=box",
            "vscode://settings/keybindings",
        ] {
            let r = route(url).unwrap();
            assert_eq!(r, Route::Vscode(url.to_string()), "{url}");
            assert!(!needs_terminal(&r), "{url}");
        }
    }

    fn assert_refused(bad: &[&str]) {
        for url in bad {
            assert!(
                route(url).is_err(),
                "{url} should be refused: {:?}",
                route(url)
            );
        }
    }

    #[test]
    fn a_croft_link_with_no_such_action_is_refused() {
        assert_refused(&["croft://bogus", "croft://run?cmd=ls"]);
    }

    /// Not handed to VS Code either: it would only report the same nothing.
    #[test]
    fn a_vscode_file_link_that_names_no_file_is_refused() {
        assert_refused(&["vscode://file/", "vscode://file"]);
    }

    #[test]
    fn a_link_in_a_scheme_the_launcher_does_not_register_is_refused() {
        assert_refused(&[
            "cursor://file//Users/me/a.rs:3",
            "https://example.com/a.rs",
            "file:///etc/hosts",
            "not a link",
        ]);
    }

    /// A stand-in for `/usr/bin/open`: `script` runs with the forwarded
    /// arguments as `$@`.
    fn sh(script: &str) -> std::process::Command {
        let mut c = std::process::Command::new("/bin/sh");
        c.args(["-c", script, "sh"]);
        c
    }

    /// `open -b` names VS Code's bundle, not the scheme: once Croft is the
    /// scheme's default, `open <url>` would hand the link straight back.
    #[test]
    fn a_link_goes_to_vs_code_by_its_bundle_id() {
        let dir = tempfile::tempdir().unwrap();
        let args = dir.path().join("args");
        let record = format!("printf '%s\\n' \"$@\" > '{}'", args.display());
        forward_to_vscode("vscode:extension/a.b", sh(&record)).unwrap();
        assert_eq!(
            std::fs::read_to_string(&args).unwrap(),
            "-b\ncom.microsoft.VSCode\nvscode:extension/a.b\n"
        );
    }

    #[test]
    fn a_link_vs_code_does_not_take_is_an_error_naming_the_link() {
        let missing = "echo 'Unable to find application named com.microsoft.VSCode' >&2; exit 1";
        let err = forward_to_vscode("vscode:extension/a.b", sh(missing))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("VS Code")
                && err.contains("vscode:extension/a.b")
                && err.contains("Unable to find application"),
            "{err}"
        );
    }
}
