//! Links to the lines on screen, on the repository's web host (#1610): Git:
//! Copy Permalink (pinned to HEAD's commit), Git: Copy Head Link (the
//! branch) and Git: Open on Remote, VS Code's GitHub extension commands.
//! [`web_repo`] reads a remote URL in any of git's spellings and
//! [`file_url`] builds the host's blob link with its own line fragment.

/// The web host a remote lives on, which decides the blob path and the
/// line fragment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forge {
    /// github.com and GitHub Enterprise (`github.<company>.com`).
    GitHub,
    /// gitlab.com and self-hosted GitLab (`gitlab.<company>.com`).
    GitLab,
    /// bitbucket.org.
    Bitbucket,
}

/// A remote read as a web repository: `https://<host>/<path>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebRepo {
    pub forge: Forge,
    /// The host, with an `https` port kept (an ssh port is not the web's).
    pub host: String,
    /// `owner/repo`, or `group/subgroup/repo` on GitLab; no `.git`.
    pub path: String,
}

/// Read `url`, a remote as `git remote get-url` prints it, as a web
/// repository: `git@host:owner/repo.git`, `ssh://git@host:22/owner/repo`,
/// `https://user@host/owner/repo.git` and `git://host/owner/repo` all name
/// the same page. `None` for a local path, or a host that is not GitHub,
/// GitLab or Bitbucket: croft cannot know another host's link shape.
pub fn web_repo(url: &str) -> Option<WebRepo> {
    let url = url.trim();
    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        let authority = authority.rsplit('@').next()?;
        let host = match scheme {
            "https" | "http" => authority,
            "ssh" | "git" | "git+ssh" | "ssh+git" => authority.split(':').next()?,
            _ => return None,
        };
        (host, path)
    } else {
        // scp-like `[user@]host:path`; a Windows drive or a path with a
        // slash before the colon is a local path.
        let (authority, path) = url.split_once(':')?;
        if authority.contains('/') || authority.len() < 2 {
            return None;
        }
        (authority.rsplit('@').next()?, path)
    };
    let host = host.to_ascii_lowercase();
    let path = path.trim_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    if host.is_empty() || path.split('/').filter(|p| !p.is_empty()).count() < 2 {
        return None;
    }
    let name = host.split(':').next().unwrap_or(&host);
    let forge = if name == "bitbucket.org" {
        Forge::Bitbucket
    } else if name.split('.').any(|label| label == "gitlab") {
        Forge::GitLab
    } else if name.split('.').any(|label| label == "github") {
        Forge::GitHub
    } else {
        return None;
    };
    Some(WebRepo {
        forge,
        host,
        path: path.to_string(),
    })
}

/// The page showing `rel` (repository-relative, `/`-separated) at `rev` (a
/// commit or a branch), scrolled to `lines`, 1-based and inclusive, in the
/// host's own fragment: GitHub `#L2-L4`, GitLab `#L2-4`, Bitbucket
/// `#lines-2:4`; one line is `#L2` (Bitbucket `#lines-2`).
pub fn file_url(repo: &WebRepo, rev: &str, rel: &str, lines: Option<(usize, usize)>) -> String {
    let blob = match repo.forge {
        Forge::GitHub => "blob",
        Forge::GitLab => "-/blob",
        Forge::Bitbucket => "src",
    };
    let fragment = match lines {
        None => String::new(),
        Some((a, b)) if a == b => match repo.forge {
            Forge::Bitbucket => format!("#lines-{a}"),
            _ => format!("#L{a}"),
        },
        Some((a, b)) => match repo.forge {
            Forge::GitHub => format!("#L{a}-L{b}"),
            Forge::GitLab => format!("#L{a}-{b}"),
            Forge::Bitbucket => format!("#lines-{a}:{b}"),
        },
    };
    format!(
        "https://{}/{}/{blob}/{}/{}{fragment}",
        repo.host,
        repo.path,
        encode_path(rev),
        encode_path(rel),
    )
}

/// Percent-encode everything in `s` a URL path cannot carry as is, keeping
/// the `/` between segments: a file named `a b#1.rs` must not end the path
/// at its `#`.
fn encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/!$&'()*+,;=:@".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github(path: &str) -> WebRepo {
        WebRepo {
            forge: Forge::GitHub,
            host: String::from("github.com"),
            path: path.to_string(),
        }
    }

    #[test]
    fn ssh_and_https_origins_name_the_same_repository() {
        let want = Some(github("acme/widgets"));
        for url in [
            "git@github.com:acme/widgets.git",
            "git@github.com:acme/widgets",
            "ssh://git@github.com/acme/widgets.git",
            "ssh://git@github.com:22/acme/widgets.git",
            "https://github.com/acme/widgets.git",
            "https://token@github.com/acme/widgets/",
            "git://github.com/acme/widgets.git",
            "  https://GitHub.com/acme/widgets  ",
        ] {
            assert_eq!(web_repo(url), want, "{url}");
        }
    }

    #[test]
    fn gitlab_bitbucket_and_enterprise_hosts_are_recognised() {
        let gl = web_repo("git@gitlab.com:group/sub/proj.git").unwrap();
        assert_eq!(
            (gl.forge, gl.path.as_str()),
            (Forge::GitLab, "group/sub/proj")
        );
        let own = web_repo("https://gitlab.example.com:8443/team/app").unwrap();
        assert_eq!(
            (own.forge, own.host.as_str()),
            (Forge::GitLab, "gitlab.example.com:8443")
        );
        let bb = web_repo("git@bitbucket.org:team/app.git").unwrap();
        assert_eq!(bb.forge, Forge::Bitbucket);
        let ghe = web_repo("git@github.acme.com:eng/core.git").unwrap();
        assert_eq!(
            (ghe.forge, ghe.host.as_str()),
            (Forge::GitHub, "github.acme.com")
        );
    }

    /// Negatives: a local path, an unknown host and a URL with no repository
    /// give no link rather than a made-up one.
    #[test]
    fn remotes_croft_cannot_link_to_give_none() {
        for url in [
            "/srv/git/widgets.git",
            "../widgets",
            "C:/repos/widgets",
            "file:///srv/git/widgets.git",
            "git@git.example.com:acme/widgets.git",
            "https://codeberg.org/acme/widgets",
            "https://github.com/acme",
            "git@github.com:",
            "",
        ] {
            assert_eq!(web_repo(url), None, "{url}");
        }
    }

    #[test]
    fn each_host_gets_its_own_line_fragment() {
        let gh = github("acme/widgets");
        assert_eq!(
            file_url(&gh, "abc123", "src/main.rs", Some((2, 4))),
            "https://github.com/acme/widgets/blob/abc123/src/main.rs#L2-L4"
        );
        assert_eq!(
            file_url(&gh, "main", "src/main.rs", Some((2, 2))),
            "https://github.com/acme/widgets/blob/main/src/main.rs#L2"
        );
        let gl = web_repo("git@gitlab.com:acme/widgets.git").unwrap();
        assert_eq!(
            file_url(&gl, "abc123", "src/main.rs", Some((2, 4))),
            "https://gitlab.com/acme/widgets/-/blob/abc123/src/main.rs#L2-4"
        );
        let bb = web_repo("git@bitbucket.org:acme/widgets.git").unwrap();
        assert_eq!(
            file_url(&bb, "abc123", "src/main.rs", Some((2, 4))),
            "https://bitbucket.org/acme/widgets/src/abc123/src/main.rs#lines-2:4"
        );
        assert_eq!(
            file_url(&bb, "abc123", "a.rs", Some((7, 7))),
            "https://bitbucket.org/acme/widgets/src/abc123/a.rs#lines-7"
        );
    }

    /// Negative: no selection means no fragment, and a path's `#`, `?` and
    /// spaces are encoded rather than ending the path.
    #[test]
    fn odd_paths_are_encoded_and_no_lines_means_no_fragment() {
        let gh = github("acme/widgets");
        assert_eq!(
            file_url(&gh, "feat/x#1", "docs/a b#1?.md", None),
            "https://github.com/acme/widgets/blob/feat/x%231/docs/a%20b%231%3F.md"
        );
    }
}
