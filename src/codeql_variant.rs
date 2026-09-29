//! CodeQL variant analysis repositories (#578): the controller repository a
//! variant analysis runs from, and the repositories, lists and owners it
//! runs against.
//!
//! The file is laid out like the `databases.json` VS Code's CodeQL
//! extension keeps for its Variant Analysis Repositories view, so a list
//! written there can be copied here: `databases.variantAnalysis` holds the
//! `repositoryLists`, `owners` and `repositories`, and `selected` says which
//! of them a run targets. VS Code keeps the controller in a setting; here it
//! sits beside them as `controllerRepository`.

#![cfg_attr(not(test), allow(dead_code))]

use std::path::Path;

/// A named list of repositories (`owner/repo`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoList {
    pub name: String,
    pub repos: Vec<String>,
}

/// What a variant analysis runs against, by name so it survives reordering.
/// The `kind` strings are VS Code's.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum Selection {
    #[serde(rename = "variantAnalysisUserDefinedList")]
    List {
        #[serde(rename = "listName")]
        list: String,
    },
    /// A repository, in list `list` or on its own.
    #[serde(rename = "variantAnalysisRepository")]
    Repo {
        #[serde(rename = "repositoryName")]
        nwo: String,
        #[serde(rename = "listName", default, skip_serializing_if = "Option::is_none")]
        list: Option<String>,
    },
    #[serde(rename = "variantAnalysisOwner")]
    Owner {
        #[serde(rename = "ownerName")]
        owner: String,
    },
}

/// One entry of the config, by position: what the side bar's rows name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    List(usize),
    /// Repository `.1` of list `.0`, or of the single repositories.
    Repo(Option<usize>, usize),
    Owner(usize),
}

/// The Variant Analysis Repositories view's contents.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(from = "ConfigFile", into = "ConfigFile")]
pub struct VariantConfig {
    pub controller_repo: Option<String>,
    pub lists: Vec<RepoList>,
    pub repos: Vec<String>,
    pub owners: Vec<String>,
    pub selected: Option<Selection>,
}

/// The on-disk shape, VS Code's `databases.json` plus the controller.
#[derive(serde::Serialize, serde::Deserialize)]
struct ConfigFile {
    #[serde(default = "config_version")]
    version: u32,
    #[serde(
        rename = "controllerRepository",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    controller: Option<String>,
    #[serde(default)]
    databases: FileDatabases,
    #[serde(
        default,
        deserialize_with = "lenient_selection",
        skip_serializing_if = "Option::is_none"
    )]
    selected: Option<Selection>,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct FileDatabases {
    #[serde(rename = "variantAnalysis", default)]
    variant_analysis: FileVariant,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct FileVariant {
    #[serde(rename = "repositoryLists", default)]
    repository_lists: Vec<FileList>,
    #[serde(default)]
    owners: Vec<String>,
    #[serde(default)]
    repositories: Vec<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct FileList {
    name: String,
    #[serde(default)]
    repositories: Vec<String>,
}

fn config_version() -> u32 {
    1
}

/// A selection croft does not know (VS Code's built-in "top 10" lists)
/// reads as none rather than making the whole file unreadable.
fn lenient_selection<'de, D>(d: D) -> Result<Option<Selection>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = <serde_json::Value as serde::Deserialize>::deserialize(d)?;
    Ok(serde_json::from_value(v).ok())
}

impl From<ConfigFile> for VariantConfig {
    fn from(f: ConfigFile) -> Self {
        let va = f.databases.variant_analysis;
        VariantConfig {
            controller_repo: f.controller,
            lists: va
                .repository_lists
                .into_iter()
                .map(|l| RepoList {
                    name: l.name,
                    repos: l.repositories,
                })
                .collect(),
            repos: va.repositories,
            owners: va.owners,
            selected: f.selected,
        }
    }
}

impl From<VariantConfig> for ConfigFile {
    fn from(c: VariantConfig) -> Self {
        ConfigFile {
            version: config_version(),
            controller: c.controller_repo,
            databases: FileDatabases {
                variant_analysis: FileVariant {
                    repository_lists: c
                        .lists
                        .into_iter()
                        .map(|l| FileList {
                            name: l.name,
                            repositories: l.repos,
                        })
                        .collect(),
                    owners: c.owners,
                    repositories: c.repos,
                },
            },
            selected: c.selected,
        }
    }
}

/// GitHub's rules for a user or organisation name, loosely: letters,
/// digits and single hyphens, not at either end, at most 39 characters.
fn valid_owner(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 39
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// GitHub's rules for a repository name, loosely: letters, digits, `.`,
/// `-` and `_`, at most 100 characters, and not `.` or `..`.
fn valid_repo(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// The path of a github.com URL (`https://github.com/a/b?x#y` → `a/b`), or
/// `None` when `value` is not one.
fn github_url_path(value: &str) -> Option<&str> {
    let rest = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .unwrap_or(value);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    const HOST: &str = "github.com/";
    let path = rest
        .get(..HOST.len())
        .filter(|h| h.eq_ignore_ascii_case(HOST))
        .map(|_| &rest[HOST.len()..])?;
    let end = path.find(['?', '#']).unwrap_or(path.len());
    Some(&path[..end])
}

/// A repository as `owner/repo`, from that or its GitHub URL
/// (`https://github.com/owner/repo`, with `.git` or a deeper path).
pub fn parse_nwo(value: &str) -> Result<String, String> {
    let value = value.trim();
    let bad = || format!("{value:?} is not a GitHub repository: use owner/repo or its URL");
    let (owner, repo) = match github_url_path(value) {
        Some(path) => {
            let mut parts = path.split('/').filter(|p| !p.is_empty());
            let owner = parts.next().ok_or_else(bad)?;
            let repo = parts.next().ok_or_else(bad)?;
            (owner, repo.strip_suffix(".git").unwrap_or(repo))
        }
        None => {
            let (owner, repo) = value.split_once('/').ok_or_else(bad)?;
            (owner, repo.strip_suffix('/').unwrap_or(repo))
        }
    };
    if !valid_owner(owner) || !valid_repo(repo) {
        return Err(bad());
    }
    Ok(format!("{owner}/{repo}"))
}

/// A GitHub user or organisation, from its name or its URL.
pub fn parse_owner(value: &str) -> Result<String, String> {
    let value = value.trim();
    let bad = || format!("{value:?} is not a GitHub owner: use a user or organisation name");
    let owner = match github_url_path(value) {
        Some(path) => {
            let mut parts = path.split('/').filter(|p| !p.is_empty());
            let owner = parts.next().ok_or_else(bad)?;
            if parts.next().is_some() {
                return Err(format!(
                    "{value:?} is a repository, not an owner: add it as a repository"
                ));
            }
            owner
        }
        None => value.strip_suffix('/').unwrap_or(value),
    };
    if !valid_owner(owner) {
        return Err(bad());
    }
    Ok(owner.to_string())
}

/// The most repositories one GitHub Code Search adds: the API serves no
/// result past its first 1000, as in VS Code's CodeQL extension.
pub const CODE_SEARCH_LIMIT: usize = 1000;

/// The Code Search query for `query`, scoped to CodeQL language `language`
/// (the side bar's) unless the query already names a language. GitHub
/// Actions workflows are YAML, which a language filter cannot tell from any
/// other YAML, so that language adds no filter.
pub fn code_search_query(query: &str, language: Option<&str>) -> String {
    let query = query.trim();
    let named = query
        .split_whitespace()
        .any(|w| w.to_ascii_lowercase().starts_with("language:"));
    match language {
        Some(lang) if !named && lang != "actions" => format!("{query} language:{lang}"),
        _ => query.to_string(),
    }
}

/// Results on one page of a Code Search: the API's most.
pub const CODE_SEARCH_PAGE: usize = 100;

/// `gh` arguments for page `page` (from 1) of a GitHub Code Search for
/// `query`, printing each result's repository as `owner/repo` on a line of
/// its own. Pages are asked for one at a time rather than with
/// `--paginate`, which would follow GitHub's link past the 1000th result
/// into a 422 and lose every page before it.
pub fn code_search_args(query: &str, page: usize) -> Vec<String> {
    [
        "api",
        "--method",
        "GET",
        "search/code",
        "--raw-field",
        &format!("q={query}"),
        "--raw-field",
        &format!("per_page={CODE_SEARCH_PAGE}"),
        "--raw-field",
        &format!("page={page}"),
        "--jq",
        ".items[].repository.full_name",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// The distinct repositories in `gh`'s output of [`code_search_args`]
/// (its pages joined), in
/// the order found and at most [`CODE_SEARCH_LIMIT`]. Many results share a
/// repository, and names differing only in letter case are one repository.
pub fn parse_code_search(out: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for nwo in out.lines().filter_map(|l| parse_nwo(l).ok()) {
        if found.len() == CODE_SEARCH_LIMIT {
            break;
        }
        if !found.iter().any(|r| r.eq_ignore_ascii_case(&nwo)) {
            found.push(nwo);
        }
    }
    found
}

impl VariantConfig {
    /// The config at `path`. A missing file is an empty config; one that
    /// cannot be read or parsed is an error, so it is never overwritten
    /// with an empty one.
    pub fn load(path: &Path) -> Result<VariantConfig, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(VariantConfig::default());
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    /// The index of the list called `name`.
    pub fn list_index(&self, name: &str) -> Option<usize> {
        self.lists.iter().position(|l| l.name == name)
    }

    /// Set the controller repository from `owner/repo` or its URL.
    pub fn set_controller(&mut self, value: &str) -> Result<String, String> {
        let nwo = parse_nwo(value)?;
        self.controller_repo = Some(nwo.clone());
        Ok(nwo)
    }

    /// Add an empty list. A blank name, or one already taken, is refused.
    pub fn add_list(&mut self, name: &str) -> Result<usize, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(String::from("A list name cannot be empty"));
        }
        if self.list_index(name).is_some() {
            return Err(format!("There is already a list called {name}"));
        }
        self.lists.push(RepoList {
            name: name.to_string(),
            repos: Vec::new(),
        });
        Ok(self.lists.len() - 1)
    }

    /// Add a repository to list `list`, or on its own. One already there
    /// (in any letter case, as GitHub names are) is refused.
    pub fn add_repo(&mut self, list: Option<usize>, value: &str) -> Result<String, String> {
        let nwo = parse_nwo(value)?;
        let (repos, place) = match list {
            Some(i) => {
                let l = self
                    .lists
                    .get_mut(i)
                    .ok_or_else(|| String::from("No such list"))?;
                (&mut l.repos, format!("list {}", l.name))
            }
            None => (&mut self.repos, String::from("the repositories")),
        };
        if repos.iter().any(|r| r.eq_ignore_ascii_case(&nwo)) {
            return Err(format!("{nwo} is already in {place}"));
        }
        repos.push(nwo.clone());
        Ok(nwo)
    }

    /// Add each of `nwos` to list `list`, skipping those already in it.
    /// Returns how many were added and how many were already there.
    pub fn add_repos(&mut self, list: usize, nwos: &[String]) -> Result<(usize, usize), String> {
        let repos = &mut self
            .lists
            .get_mut(list)
            .ok_or_else(|| String::from("No such list"))?
            .repos;
        let before = repos.len();
        for nwo in nwos {
            if !repos.iter().any(|r| r.eq_ignore_ascii_case(nwo)) {
                repos.push(nwo.clone());
            }
        }
        let added = repos.len() - before;
        Ok((added, nwos.len() - added))
    }

    /// Add an owner, all of whose repositories a run targets.
    pub fn add_owner(&mut self, value: &str) -> Result<String, String> {
        let owner = parse_owner(value)?;
        if self.owners.iter().any(|o| o.eq_ignore_ascii_case(&owner)) {
            return Err(format!("{owner} is already listed"));
        }
        self.owners.push(owner.clone());
        Ok(owner)
    }

    /// Rename list `index`; the selection follows it.
    pub fn rename_list(&mut self, index: usize, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(String::from("A list name cannot be empty"));
        }
        let old = self
            .lists
            .get(index)
            .map(|l| l.name.clone())
            .ok_or_else(|| String::from("No such list"))?;
        if old == name {
            return Ok(());
        }
        if self.list_index(name).is_some() {
            return Err(format!("There is already a list called {name}"));
        }
        self.lists[index].name = name.to_string();
        match &mut self.selected {
            Some(Selection::List { list })
            | Some(Selection::Repo {
                list: Some(list), ..
            }) if *list == old => {
                *list = name.to_string();
            }
            _ => {}
        }
        Ok(())
    }

    /// The selection `item` stands for, when it exists.
    fn selection_of(&self, item: Item) -> Option<Selection> {
        Some(match item {
            Item::List(i) => Selection::List {
                list: self.lists.get(i)?.name.clone(),
            },
            Item::Repo(Some(i), j) => {
                let l = self.lists.get(i)?;
                Selection::Repo {
                    nwo: l.repos.get(j)?.clone(),
                    list: Some(l.name.clone()),
                }
            }
            Item::Repo(None, j) => Selection::Repo {
                nwo: self.repos.get(j)?.clone(),
                list: None,
            },
            Item::Owner(i) => Selection::Owner {
                owner: self.owners.get(i)?.clone(),
            },
        })
    }

    /// The display name of `item`.
    pub fn name_of(&self, item: Item) -> Option<&str> {
        match item {
            Item::List(i) => self.lists.get(i).map(|l| l.name.as_str()),
            Item::Repo(Some(i), j) => self.lists.get(i)?.repos.get(j).map(String::as_str),
            Item::Repo(None, j) => self.repos.get(j).map(String::as_str),
            Item::Owner(i) => self.owners.get(i).map(String::as_str),
        }
    }

    pub fn is_selected(&self, item: Item) -> bool {
        self.selected.is_some() && self.selection_of(item) == self.selected
    }

    /// Make `item` what a variant analysis runs against.
    pub fn select(&mut self, item: Item) -> Result<(), String> {
        let sel = self
            .selection_of(item)
            .ok_or_else(|| String::from("That entry is no longer listed"))?;
        self.selected = Some(sel);
        Ok(())
    }

    /// Remove `item`, clearing the selection when it was, or was inside,
    /// what went.
    pub fn remove(&mut self, item: Item) -> Result<(), String> {
        let gone = self
            .selection_of(item)
            .ok_or_else(|| String::from("That entry is no longer listed"))?;
        match item {
            Item::List(i) => {
                self.lists.remove(i);
            }
            Item::Repo(Some(i), j) => {
                self.lists[i].repos.remove(j);
            }
            Item::Repo(None, j) => {
                self.repos.remove(j);
            }
            Item::Owner(i) => {
                self.owners.remove(i);
            }
        }
        let clear = match (&self.selected, &gone) {
            (Some(s), _) if *s == gone => true,
            (Some(Selection::Repo { list: Some(l), .. }), Selection::List { list }) => l == list,
            _ => false,
        };
        if clear {
            self.selected = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_nwo_takes_owner_repo_and_github_urls() {
        assert_eq!(parse_nwo("github/codeql").unwrap(), "github/codeql");
        assert_eq!(parse_nwo("  a-b/c.d_e  ").unwrap(), "a-b/c.d_e");
        for url in [
            "https://github.com/github/codeql",
            "https://github.com/github/codeql/",
            "https://github.com/github/codeql.git",
            "http://www.github.com/github/codeql/tree/main/ql",
            "github.com/github/codeql?tab=readme#top",
        ] {
            assert_eq!(parse_nwo(url).unwrap(), "github/codeql", "{url}");
        }
    }

    #[test]
    fn parse_nwo_refuses_what_github_would() {
        for bad in [
            "",
            "codeql",
            "/codeql",
            "github/",
            "-github/codeql",
            "github-/codeql",
            "git hub/codeql",
            "github/code ql",
            "github/..",
            "a/b/c",
            "https://github.com/github",
            "https://gitlab.com/github/codeql",
            &format!("{}/x", "a".repeat(40)),
        ] {
            assert!(parse_nwo(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parse_owner_takes_a_name_or_its_url() {
        assert_eq!(parse_owner("octo-org").unwrap(), "octo-org");
        assert_eq!(
            parse_owner("https://github.com/octo-org/").unwrap(),
            "octo-org"
        );
        assert!(parse_owner("octo org").is_err());
        assert!(parse_owner("").is_err());
        assert!(parse_owner("octo/repo").is_err());
        assert!(
            parse_owner("https://github.com/octo/repo")
                .unwrap_err()
                .contains("add it as a repository")
        );
    }

    #[test]
    fn reads_a_vs_code_databases_json_and_round_trips() {
        let text = r#"{
            "version": 1,
            "databases": {
                "variantAnalysis": {
                    "repositoryLists": [
                        {"name": "new-repo-list", "repositories": ["a/b", "c/d"]}
                    ],
                    "owners": ["octo-org"],
                    "repositories": ["e/f"]
                },
                "local": {"lists": [], "databases": []}
            },
            "selected": {"kind": "variantAnalysisRepository",
                         "repositoryName": "c/d", "listName": "new-repo-list"}
        }"#;
        let c: VariantConfig = serde_json::from_str(text).unwrap();
        assert_eq!(c.lists[0].repos, ["a/b", "c/d"]);
        assert_eq!(c.owners, ["octo-org"]);
        assert_eq!(c.repos, ["e/f"]);
        assert!(c.is_selected(Item::Repo(Some(0), 1)));
        assert!(!c.is_selected(Item::Repo(None, 0)));
        assert_eq!(c.controller_repo, None);

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sub").join("va.json");
        let mut c = c;
        c.set_controller("https://github.com/me/ctl").unwrap();
        c.save(&path).unwrap();
        let back = VariantConfig::load(&path).unwrap();
        assert_eq!(back, c);
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["version"], 1);
        assert_eq!(raw["controllerRepository"], "me/ctl");
        assert_eq!(
            raw["databases"]["variantAnalysis"]["repositoryLists"][0]["name"],
            "new-repo-list"
        );
        assert_eq!(raw["selected"]["kind"], "variantAnalysisRepository");
    }

    #[test]
    fn a_missing_file_is_empty_and_a_corrupt_one_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("va.json");
        assert_eq!(
            VariantConfig::load(&path).unwrap(),
            VariantConfig::default()
        );
        std::fs::write(&path, "{ not json").unwrap();
        assert!(VariantConfig::load(&path).is_err());
        // An unknown selection kind (VS Code's built-in lists) is dropped,
        // not an error.
        std::fs::write(
            &path,
            r#"{"selected":{"kind":"variantAnalysisSystemDefinedList","listName":"top_10"}}"#,
        )
        .unwrap();
        assert_eq!(VariantConfig::load(&path).unwrap().selected, None);
    }

    #[test]
    fn duplicates_are_refused() {
        let mut c = VariantConfig::default();
        c.add_list("mine").unwrap();
        assert!(c.add_list(" mine ").unwrap_err().contains("already"));
        assert!(c.add_list("  ").is_err());
        c.add_repo(Some(0), "a/b").unwrap();
        assert!(c.add_repo(Some(0), "A/B").unwrap_err().contains("already"));
        c.add_repo(None, "a/b").unwrap();
        assert!(c.add_repo(None, "https://github.com/a/b").is_err());
        assert!(c.add_repo(Some(5), "a/c").is_err());
        c.add_owner("octo").unwrap();
        assert!(c.add_owner("Octo").unwrap_err().contains("already"));
    }

    #[test]
    fn rename_select_and_remove_keep_the_selection_honest() {
        let mut c = VariantConfig::default();
        c.add_list("one").unwrap();
        c.add_list("two").unwrap();
        c.add_repo(Some(0), "a/b").unwrap();
        c.add_repo(None, "c/d").unwrap();
        c.add_owner("octo").unwrap();

        c.select(Item::Repo(Some(0), 0)).unwrap();
        c.rename_list(0, "uno").unwrap();
        assert!(c.is_selected(Item::Repo(Some(0), 0)), "follows the rename");
        assert!(c.rename_list(0, "two").is_err(), "taken");
        assert!(c.rename_list(0, "").is_err());
        c.rename_list(0, "uno").unwrap();

        // Removing the list the selected repository is in clears it.
        c.remove(Item::List(0)).unwrap();
        assert_eq!(c.selected, None);
        assert_eq!(c.lists.len(), 1);

        c.select(Item::Owner(0)).unwrap();
        c.remove(Item::Repo(None, 0)).unwrap();
        assert!(c.is_selected(Item::Owner(0)), "another removal keeps it");
        c.remove(Item::Owner(0)).unwrap();
        assert_eq!(c.selected, None);
        assert!(c.remove(Item::Owner(0)).is_err());
        assert!(c.select(Item::List(3)).is_err());
        assert_eq!(c.name_of(Item::List(0)), Some("two"));
    }

    #[test]
    fn a_code_search_is_scoped_to_the_side_bars_language() {
        assert_eq!(
            code_search_query(" import torch ", Some("python")),
            "import torch language:python"
        );
        assert_eq!(code_search_query("x", None), "x");
        // A query naming its own language keeps it.
        assert_eq!(
            code_search_query("x Language:Go", Some("python")),
            "x Language:Go"
        );
        // A language filter cannot single out workflow YAML.
        assert_eq!(code_search_query("on: push", Some("actions")), "on: push");
        let args = code_search_args("a b language:go", 3);
        assert!(
            args.contains(&String::from("q=a b language:go")),
            "{args:?}"
        );
        assert!(args.contains(&String::from("page=3")), "{args:?}");
        assert!(!args.contains(&String::from("--paginate")), "{args:?}");
    }

    #[test]
    fn code_search_results_become_distinct_repositories_up_to_the_cap() {
        let out = "github/codeql\nGitHub/CodeQL\n\nnot a repo\na/b\ngithub/codeql\n";
        assert_eq!(parse_code_search(out), ["github/codeql", "a/b"]);
        let many: String = (0..CODE_SEARCH_LIMIT + 5)
            .map(|i| format!("o/r{i}\n"))
            .collect();
        let found = parse_code_search(&many);
        assert_eq!(found.len(), CODE_SEARCH_LIMIT);
        assert_eq!(found.last().map(String::as_str), Some("o/r999"));
    }

    #[test]
    fn adding_search_results_skips_repositories_already_listed() {
        let mut c = VariantConfig::default();
        c.add_list("top").unwrap();
        c.add_repo(Some(0), "a/b").unwrap();
        let found = vec![String::from("A/B"), String::from("c/d")];
        assert_eq!(c.add_repos(0, &found), Ok((1, 1)));
        assert_eq!(c.lists[0].repos, ["a/b", "c/d"]);
        assert!(c.add_repos(1, &found).is_err());
    }
}
