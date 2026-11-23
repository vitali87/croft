//! The command line's own tests, moved out of cli.rs (#2062).

use super::*;
use clap::Parser;

#[test]
fn pr_takes_a_number_or_a_url() {
    let cli = Cli::try_parse_from(["croft", "pr", "#579"]).unwrap();
    assert!(matches!(cli.command, Some(CliCommand::Pr { ref number }) if number == "#579"));
    assert!(
        Cli::try_parse_from(["croft", "pr"]).is_err(),
        "the number is required"
    );
}

#[test]
fn demo_is_a_subcommand() {
    let cli = Cli::try_parse_from(["croft", "demo"]).unwrap();
    assert!(matches!(cli.command, Some(CliCommand::Demo { tour: None })));
    let cli = Cli::try_parse_from(["croft", "demo", "--tour", "mine.json"]).unwrap();
    assert!(
        matches!(cli.command, Some(CliCommand::Demo { tour: Some(p) }) if p == Path::new("mine.json"))
    );
}

/// #375: `--build-info` says when a package manager owns the binary, so a
/// bug report shows which upgrade path the user has.
#[test]
fn build_info_names_a_homebrew_install() {
    use crate::update_check::InstallSource;
    let brew = build_info_line(InstallSource::Homebrew);
    assert!(
        brew.starts_with(VERBOSE_VERSION.trim_end_matches(')')),
        "{brew}"
    );
    assert!(brew.ends_with(", installed with Homebrew)"), "{brew}");
    assert_eq!(build_info_line(InstallSource::SelfManaged), VERBOSE_VERSION);
}

/// #282: `--version` is a plain `x.y.z`. The regression this guards is a
/// well-meaning one — re-adding provenance "so bug reports carry it" is
/// exactly how the hash got into the common path the first time.
#[test]
fn version_is_a_bare_semver_and_provenance_lives_behind_build_info() {
    // Assert on what clap RENDERS, not on the constant. The binding under
    // test is the `version = VERSION` token in the `#[command(...)]`
    // attribute, and regressing that alone leaves every constant correct:
    // a check against VERSION would pass while `--version` printed the
    // hash again, which is exactly the regression this guards.
    let rendered = <Cli as clap::CommandFactory>::command().render_version();
    assert_eq!(
        rendered.trim(),
        format!("croft {}", env!("CARGO_PKG_VERSION")),
        "--version must render a bare x.y.z"
    );
    assert!(
        !rendered.contains('('),
        "provenance must not ride along on --version: {rendered:?}"
    );

    assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));

    // The provenance did not disappear, it moved.
    assert!(VERBOSE_VERSION.starts_with(VERSION));
    assert!(
        VERBOSE_VERSION.contains(env!("CROFT_GIT_HASH")),
        "--build-info still carries the short hash: {VERBOSE_VERSION:?}"
    );

    let cli = Cli::try_parse_from(["croft", "--build-info"]).unwrap();
    assert!(cli.build_info);
    assert!(!Cli::try_parse_from(["croft"]).unwrap().build_info);
}

/// `--build-info`'s entry in rendered `help`, from its flag to the line
/// that opens the next option (`-x, --flag` or `--flag`), or to the end.
fn build_info_entry(help: &str) -> &str {
    let start = help.find("--build-info").expect("--build-info is listed");
    let entry = &help[start..];
    let mut end = entry.find('\n').map_or(entry.len(), |i| i + 1);
    for line in entry[end..].split_inclusive('\n') {
        if line.trim_start().starts_with('-') {
            break;
        }
        end += line.len();
    }
    &entry[..end]
}

/// #853 review: the entry ends where the next option starts, whichever
/// option that is, so reordering or renaming the flags around
/// `--build-info` neither breaks these tests nor lets them read past it.
#[test]
fn the_build_info_entry_ends_at_whichever_option_follows_it() {
    let help = "Options:\n      --build-info\n          Print build provenance and exit.\n\n          Ignores that subcommand.\n\n      --zzz <WHAT>\n          Talks about clap internals.\n";
    let entry = build_info_entry(help);
    assert!(entry.contains("Ignores that subcommand."), "{entry:?}");
    assert!(!entry.contains("--zzz"), "{entry:?}");
    assert!(!entry.contains("clap"), "{entry:?}");
    // The last option runs to the end of the page.
    let last =
        "Options:\n  -V, --version  Print version\n      --build-info  Print build provenance\n";
    assert_eq!(
        build_info_entry(last),
        "--build-info  Print build provenance\n"
    );
    // -h's one-line form, and a short flag opening the next entry.
    let short =
        "      --build-info  Print build provenance and exit\n  -o, --open-file <F>  Open F\n";
    assert_eq!(
        build_info_entry(short),
        "--build-info  Print build provenance and exit\n"
    );
}

/// #853: clap prints a field's whole `///` doc as its long help, so a
/// maintainer's note about clap left there shipped in `croft --help`.
#[test]
fn build_info_long_help_speaks_to_users_not_maintainers() {
    let help = <Cli as clap::CommandFactory>::command()
        .render_long_help()
        .to_string();
    let entry = build_info_entry(&help);
    assert!(
        entry.contains("ignores that subcommand"),
        "the user-facing caveat stays: {entry}"
    );
    for leak in ["clap", "conflicts_with", "assert", "(#"] {
        assert!(
            !entry.contains(leak),
            "{leak:?} leaked into --build-info's help: {entry}"
        );
    }
}

/// #853 guard: `-h` shows only a field's first paragraph, so the caveat
/// (and anything a maintainer might tuck after it) stays out of the
/// short help; `--build-info` there is its one-line summary.
#[test]
fn build_info_short_help_is_its_summary_alone() {
    let help = <Cli as clap::CommandFactory>::command()
        .render_help()
        .to_string();
    let entry = build_info_entry(&help);
    assert!(entry.contains("build provenance"), "{entry}");
    for absent in ["subcommand", "clap", "conflicts_with", "(#"] {
        assert!(
            !entry.contains(absent),
            "{absent:?} does not belong in -h's --build-info line: {entry}"
        );
    }
}

/// #853 guard: moving the note did not just move the leak. No help page,
/// the top level or any subcommand's, talks about clap internals, and
/// (#1172) none carries a maintainer's note about Rust types or cites an
/// issue number, which reads as an unexplained footnote.
#[test]
fn no_help_page_carries_a_note_about_clap_internals() {
    // A backticked `#579` is an example of input (`croft pr`), not a
    // citation.
    fn issue_ref(help: &str) -> Option<&str> {
        help.match_indices('#').find_map(|(i, _)| {
            let digits = help[i + 1..].bytes().take_while(u8::is_ascii_digit).count();
            let quoted = help[..i].ends_with('`');
            (digits >= 3 && !quoted).then(|| &help[i..i + 1 + digits])
        })
    }
    fn walk(cmd: &mut clap::Command, path: &str) {
        let help = cmd.render_long_help().to_string();
        for leak in [
            "clap",
            "conflicts_with",
            "runtime assert",
            "OsString",
            "`String`",
        ] {
            assert!(
                !help.contains(leak),
                "{leak:?} leaked into `{path} --help`: {help}"
            );
        }
        if let Some(cited) = issue_ref(&help) {
            panic!("`{path} --help` cites issue {cited}: {help}");
        }
        for sub in cmd.get_subcommands_mut() {
            let path = format!("{path} {}", sub.get_name());
            walk(sub, &path);
        }
    }
    assert_eq!(issue_ref("the tour (#377) in"), Some("#377"));
    assert_eq!(issue_ref("PR `#579`, #57, #ab, #"), None);
    let mut cmd = <Cli as clap::CommandFactory>::command();
    cmd.build();
    walk(&mut cmd, "croft");
}

/// #853 guard against over-correcting the caveat into a clap rule:
/// `conflicts_with = "command"` names no argument id, so clap would
/// panic on it. `--build-info` beside a subcommand keeps parsing, and
/// `run` answers `--build-info` first, as the help now says.
#[test]
fn build_info_beside_a_subcommand_still_parses() {
    <Cli as clap::CommandFactory>::command().debug_assert();
    let cli = Cli::try_parse_from(["croft", "--build-info", "demo"]).unwrap();
    assert!(cli.build_info);
    assert!(matches!(cli.command, Some(CliCommand::Demo { tour: None })));
}

fn typo_error(args: &[&str], base: &Path) -> Option<String> {
    let args: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
    misspelled_subcommand(&args, base).map(|e| {
        assert_eq!(e.exit_code(), 2, "a usage error, like clap's own");
        e.to_string()
    })
}

#[test]
fn a_misspelled_subcommand_is_suggested_not_opened_as_a_folder() {
    let dir = tempfile::tempdir().unwrap();
    let msg = typo_error(&["croft", "atach"], dir.path()).expect("a typo error");
    assert!(
        msg.contains("'atach' is not a croft command or an existing path"),
        "{msg}"
    );
    assert!(
        msg.contains("a similar subcommand exists: 'attach'"),
        "{msg}"
    );
    let msg = typo_error(&["croft", "locale-templat", "fr"], dir.path()).expect("a typo error");
    assert!(msg.contains("'locale-template'"), "{msg}");
}

#[test]
fn the_suggestion_is_for_the_misspelled_word_not_the_one_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let msg = typo_error(&["croft", "sync-confg", "devbox"], dir.path()).expect("a typo error");
    assert!(msg.contains("'sync-config'"), "{msg}");
    assert!(!msg.contains("'demo'"), "{msg}");
    // Flags before the word, and a flag's value, are not the word.
    let msg = typo_error(
        &["croft", "--zen", "--open-file", "x.rs", "atach"],
        dir.path(),
    )
    .expect("a typo error");
    assert!(msg.contains("'attach'"), "{msg}");
}

#[test]
fn an_existing_folder_named_like_a_typo_still_opens() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("atach")).unwrap();
    assert_eq!(typo_error(&["croft", "atach"], dir.path()), None);
}

#[test]
fn words_that_are_not_near_a_subcommand_are_left_to_clap() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        &["croft"][..],
        &["croft", "attach"],
        &["croft", "sync-config", "devbox"],
        &["croft", "--open-file", "atach"],
        &["croft", "app"],
        &["croft", "my-project"],
        &["croft", "sub/atach"],
        &["croft", "--", "atach"],
    ] {
        assert_eq!(typo_error(args, dir.path()), None, "{args:?}");
    }
}

#[test]
fn a_missing_workspace_path_is_named_in_the_error() {
    let dir = tempfile::tempdir().unwrap();
    // Its folder is missing too: a new file there has nowhere to go.
    let missing = dir.path().join("missing-dir/notes.md");
    let err = resolve_workspace(&missing, None).unwrap_err();
    assert!(
        format!("{err:#}").contains(&missing.display().to_string()),
        "{err:#}"
    );
}

/// #1157: `croft notes.md` for a file not written yet roots at its
/// folder and opens it, as `code notes.md` and `vim notes.md` do, from
/// the cwd or by absolute path.
#[test]
fn a_file_not_written_yet_roots_at_its_folder_and_opens() {
    let dir = tempfile::tempdir().unwrap();
    let canon = dir.path().canonicalize().unwrap();
    let (root, open, folders) =
        resolve_workspace_from(Path::new("notes.md"), None, dir.path()).unwrap();
    assert_eq!(root, canon);
    assert_eq!(open, Some(canon.join("notes.md")));
    assert!(folders.is_empty());
    let (root, open, _) = resolve_workspace(&dir.path().join("todo.txt"), None).unwrap();
    assert_eq!(root, canon);
    assert_eq!(open, Some(canon.join("todo.txt")));
    assert!(
        !canon.join("notes.md").exists(),
        "nothing is created on disk"
    );
    assert!(
        !canon.join("todo.txt").exists(),
        "nothing is created on disk"
    );
}

/// #1157, negative: a missing folder, a path written as a folder, and a
/// path that ends in `..` are still refused.
#[test]
fn a_new_file_needs_an_existing_folder_and_a_name() {
    let dir = tempfile::tempdir().unwrap();
    for typed in ["no-such-dir/notes.md", "new-dir/", "no-such-dir/.."] {
        let err = resolve_workspace_from(Path::new(typed), None, dir.path()).unwrap_err();
        assert!(
            format!("{err:#}").contains("resolving workspace path"),
            "{typed}: {err:#}"
        );
    }
}

#[test]
fn file_path_roots_the_workspace_at_its_parent_and_opens_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("pitch_deck.tex");
    std::fs::write(&file, "hi").unwrap();
    let (root, open, _) = resolve_workspace(&file, None).unwrap();
    assert_eq!(root, dir.path().canonicalize().unwrap());
    assert_eq!(open, Some(file.canonicalize().unwrap()));
}

#[test]
fn directory_path_stays_the_workspace_root() {
    let dir = tempfile::tempdir().unwrap();
    let (root, open, _) = resolve_workspace(dir.path(), None).unwrap();
    assert_eq!(root, dir.path().canonicalize().unwrap());
    assert_eq!(open, None);
}

#[test]
fn resolve_workspace_opens_a_code_workspace_file_as_a_folder_set() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("alpha");
    let b = dir.path().join("beta");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let file = dir.path().join("p.code-workspace");
    crate::workspace::write_workspace_file(&file, &[a.clone(), b.clone()]).unwrap();
    let (root, open, folders) = resolve_workspace(&file, None).unwrap();
    assert_eq!(root, a.canonicalize().unwrap(), "first folder is primary");
    assert_eq!(open, None, "a workspace file is not an open-file");
    assert_eq!(folders, vec![b.canonicalize().unwrap()]);

    // A malformed workspace file is a launch error, not a text open.
    let bad = dir.path().join("bad.code-workspace");
    std::fs::write(&bad, "{ nope").unwrap();
    assert!(resolve_workspace(&bad, None).is_err());
}

/// #1606: `croft big.txt:150` roots the workspace at big.txt's folder and
/// opens it at line 150; `--open-file big.txt:150:3` adds the column.
#[test]
fn a_file_line_launch_argument_opens_the_file_at_that_line() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::write(root.join("big.txt"), "a\nb\n").unwrap();
    let (path, open_file, at) = split_launch_location(Path::new("big.txt:150"), None, &root);
    assert_eq!(at, Some((149, 0)));
    let (ws, open_file, _) = resolve_workspace_from(&path, open_file, &root).unwrap();
    assert_eq!(ws, root);
    assert_eq!(open_file, Some(root.join("big.txt")));

    let (path, open_file, at) =
        split_launch_location(Path::new("."), Some(PathBuf::from("big.txt:150:3")), &root);
    assert_eq!(at, Some((149, 2)));
    let (ws, open_file, _) = resolve_workspace_from(&path, open_file, &root).unwrap();
    assert_eq!(ws, root);
    assert_eq!(open_file, Some(root.join("big.txt")));
}

/// #1606: a relative `--open-file` location under the workspace, launched
/// from elsewhere, is found there.
#[test]
fn an_open_file_location_is_found_under_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    let (_, open_file, at) =
        split_launch_location(&root, Some(PathBuf::from("src/main.rs:1:4")), dir.path());
    assert_eq!(open_file, Some(root.join("src/main.rs")));
    assert_eq!(at, Some((0, 3)));
}

/// #1606: with a `.code-workspace` file as PATH, a relative
/// `--open-file` location is found under the workspace's primary folder.
#[test]
fn an_open_file_location_is_found_under_a_workspace_files_primary_folder() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path().canonicalize().unwrap();
    let primary = dir.join("alpha");
    std::fs::create_dir_all(primary.join("src")).unwrap();
    std::fs::write(primary.join("src/main.rs"), "fn main() {}\n").unwrap();
    let file = dir.join("p.code-workspace");
    crate::workspace::write_workspace_file(&file, std::slice::from_ref(&primary)).unwrap();
    let (path, open_file, at) =
        split_launch_location(&file, Some(PathBuf::from("src/main.rs:10")), &dir);
    assert_eq!(path, file);
    assert_eq!(open_file, Some(primary.join("src/main.rs")));
    assert_eq!(at, Some((9, 0)));
}

/// Negative (#1606): a path that exists, even one named like `a:1`, is
/// left alone and carries no location; an explicit `--open-file` wins
/// over the PATH argument's location. A missing file keeps its suffix as
/// part of its name, so it opens as a new file of that name (#1157), and
/// a missing folder is still refused.
#[test]
fn an_existing_path_or_an_explicit_open_file_keeps_its_meaning() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::write(root.join("a:1"), "x\n").unwrap();
    std::fs::write(root.join("big.txt"), "a\n").unwrap();
    assert_eq!(
        split_launch_location(Path::new("a:1"), None, &root),
        (PathBuf::from("a:1"), None, None)
    );
    assert_eq!(
        split_launch_location(Path::new("."), None, &root),
        (PathBuf::from("."), None, None)
    );
    let (_, open_file, at) =
        split_launch_location(Path::new("big.txt:9"), Some(PathBuf::from("a:1")), &root);
    assert_eq!((open_file, at), (Some(PathBuf::from("a:1")), None));
    assert_eq!(
        split_launch_location(Path::new("nope.txt:9"), None, &root),
        (PathBuf::from("nope.txt:9"), None, None)
    );
    let (_, open, _) = resolve_workspace_from(Path::new("nope.txt:9"), None, &root).unwrap();
    assert_eq!(open, Some(root.join("nope.txt:9")));
    assert!(resolve_workspace_from(Path::new("nope/x.txt:9"), None, &root).is_err());
}

#[test]
fn explicit_open_file_wins_over_the_derived_one() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.tex");
    std::fs::write(&file, "hi").unwrap();
    let other = dir.path().join("b.rs");
    let (root, open, _) = resolve_workspace(&file, Some(other.clone())).unwrap();
    assert_eq!(root, dir.path().canonicalize().unwrap());
    assert_eq!(open, Some(other));
}

/// `croft . --open-file pkg/util.py` and `croft rn --open-file
/// pkg/util.py` from the parent both open the absolute file, so its tab
/// gets a language server (#1193).
#[test]
fn a_relative_open_file_is_made_absolute_under_the_cwd_or_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("rn");
    std::fs::create_dir_all(ws.join("pkg")).unwrap();
    std::fs::write(ws.join("pkg/util.py"), "x = 1\n").unwrap();
    let util = ws.join("pkg/util.py").canonicalize().unwrap();
    let rel = Some(PathBuf::from("pkg/util.py"));
    let (_, open, _) = resolve_workspace_from(Path::new("."), rel.clone(), &ws).unwrap();
    assert_eq!(open, Some(util.clone()), "from the workspace root");
    let (root, open, _) = resolve_workspace_from(Path::new("rn"), rel, dir.path()).unwrap();
    assert_eq!(root, ws.canonicalize().unwrap());
    assert_eq!(open, Some(util), "from another directory");
}

/// Negative: a relative path that names a file under the cwd keeps
/// meaning that file, an absolute path is left alone, and a file that
/// doesn't exist yet is still anchored to the workspace for the launch
/// to report.
#[test]
fn an_open_file_under_the_cwd_or_absolute_or_missing_keeps_its_meaning() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(dir.path().join("notes.md"), "").unwrap();
    std::fs::write(ws.join("notes.md"), "").unwrap();
    let (_, open, _) =
        resolve_workspace_from(Path::new("ws"), Some(PathBuf::from("notes.md")), dir.path())
            .unwrap();
    assert_eq!(
        open,
        Some(dir.path().join("notes.md").canonicalize().unwrap())
    );
    let abs = ws.join("notes.md").canonicalize().unwrap();
    let (_, open, _) =
        resolve_workspace_from(Path::new("ws"), Some(abs.clone()), dir.path()).unwrap();
    assert_eq!(open, Some(abs));
    let (root, open, _) =
        resolve_workspace_from(Path::new("ws"), Some(PathBuf::from("new.rs")), dir.path()).unwrap();
    assert_eq!(open, Some(root.join("new.rs")));
}

#[test]
fn parses_web_with_and_without_a_bind() {
    let cli = Cli::try_parse_from(["croft", "web"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(CliCommand::Web {
            workspace: None,
            bind: None,
            tls: None,
            rotate_token: false,
            off: false,
        })
    ));
    let cli = Cli::try_parse_from([
        "croft",
        "web",
        "/w",
        "--bind",
        "0.0.0.0:9000",
        "--tls",
        "c.pem",
        "k.pem",
    ])
    .unwrap();
    match cli.command {
        Some(CliCommand::Web {
            workspace,
            bind,
            tls,
            ..
        }) => {
            assert_eq!(workspace.as_deref(), Some(std::path::Path::new("/w")));
            assert_eq!(bind, Some("0.0.0.0:9000".parse().unwrap()));
            assert_eq!(
                tls,
                Some(vec![PathBuf::from("c.pem"), PathBuf::from("k.pem")])
            );
        }
        other => panic!("{other:?}"),
    }
    let cli = Cli::try_parse_from(["croft", "web", "--rotate-token"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(CliCommand::Web {
            rotate_token: true,
            ..
        })
    ));
    // `--tls` takes both files; `--off` stops, so it takes no listener
    // settings and cannot also rotate.
    assert!(Cli::try_parse_from(["croft", "web", "--tls", "c.pem"]).is_err());
    assert!(Cli::try_parse_from(["croft", "web", "--off", "--bind", "127.0.0.1:1"]).is_err());
    assert!(Cli::try_parse_from(["croft", "web", "--off", "--rotate-token"]).is_err());
    assert!(matches!(
        Cli::try_parse_from(["croft", "web", "--off"])
            .unwrap()
            .command,
        Some(CliCommand::Web { off: true, .. })
    ));
}

#[test]
fn parses_hook_install_and_the_hook_itself() {
    let cli = Cli::try_parse_from(["croft", "hook", "install", "--global", "--uninstall"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(CliCommand::Hook {
            action: HookAction::Install {
                global: true,
                project: false,
                uninstall: true
            }
        })
    ));
    let cli = Cli::try_parse_from(["croft", "hook", "claude-code"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(CliCommand::Hook {
            action: HookAction::ClaudeCode
        })
    ));
    assert!(Cli::try_parse_from(["croft", "hook", "install", "--global", "--project"]).is_err());
}

#[test]
fn parses_no_args() {
    let cli = Cli::parse_from(["croft"]);
    assert!(cli.path.is_none());
    assert!(cli.command.is_none());
}

#[test]
fn parses_path_argument() {
    let cli = Cli::parse_from(["croft", "/tmp"]);
    assert_eq!(cli.path, Some(PathBuf::from("/tmp")));
    assert!(cli.command.is_none());
}

#[test]
fn parses_setup_terminal_subcommand_with_defaults() {
    let cli = Cli::parse_from(["croft", "setup-terminal"]);
    assert!(cli.path.is_none());
    match cli.command {
        Some(CliCommand::SetupTerminal { font, size, yes }) => {
            assert_eq!(font, SETUP_FONT_PS_NAME);
            assert_eq!(size, SETUP_FONT_SIZE);
            assert!(!yes);
        }
        _ => panic!("expected SetupTerminal"),
    }
}

#[test]
fn parses_setup_terminal_with_overrides() {
    let cli = Cli::parse_from([
        "croft",
        "setup-terminal",
        "--font",
        "FiraCodeNFM-Regular",
        "--size",
        "14",
        "--yes",
    ]);
    match cli.command {
        Some(CliCommand::SetupTerminal { font, size, yes }) => {
            assert_eq!(font, "FiraCodeNFM-Regular");
            assert_eq!(size, 14);
            assert!(yes);
        }
        _ => panic!("expected SetupTerminal"),
    }
}

#[test]
fn parses_setup_terminal_with_short_yes() {
    let cli = Cli::parse_from(["croft", "setup-terminal", "-y"]);
    match cli.command {
        Some(CliCommand::SetupTerminal { yes, .. }) => assert!(yes),
        _ => panic!("expected SetupTerminal"),
    }
}

#[test]
fn keys_labels_every_event_kind() {
    use crossterm::event::KeyEventKind;
    assert_eq!(key_kind_label(KeyEventKind::Press), "Press");
    assert_eq!(key_kind_label(KeyEventKind::Repeat), "Repeat");
    assert_eq!(key_kind_label(KeyEventKind::Release), "Release");
}

#[test]
fn parses_keys_subcommand() {
    let cli = Cli::parse_from(["croft", "keys"]);
    assert!(matches!(
        cli.command,
        Some(CliCommand::Keys { mouse: false })
    ));
}

#[test]
fn parses_keys_subcommand_with_mouse() {
    let cli = Cli::parse_from(["croft", "keys", "--mouse"]);
    assert!(matches!(
        cli.command,
        Some(CliCommand::Keys { mouse: true })
    ));
}

#[test]
fn parses_setup_iterm2_with_defaults() {
    let cli = Cli::parse_from(["croft", "setup-iterm2"]);
    match cli.command {
        Some(CliCommand::SetupIterm2 {
            font,
            nonascii,
            size,
            yes,
        }) => {
            assert_eq!(font, ITERM2_FONT_PS_NAME);
            assert_eq!(nonascii, ITERM2_NONASCII_PS_NAME);
            assert_eq!(size, ITERM2_FONT_SIZE);
            assert!(!yes);
        }
        _ => panic!("expected SetupIterm2"),
    }
}

#[test]
fn parses_setup_iterm2_with_overrides() {
    let cli = Cli::parse_from([
        "croft",
        "setup-iterm2",
        "--font",
        "FiraCodeNFM-Reg",
        "--nonascii",
        "SymbolsNFM",
        "--size",
        "15",
        "-y",
    ]);
    match cli.command {
        Some(CliCommand::SetupIterm2 {
            font,
            nonascii,
            size,
            yes,
        }) => {
            assert_eq!(font, "FiraCodeNFM-Reg");
            assert_eq!(nonascii, "SymbolsNFM");
            assert_eq!(size, 15);
            assert!(yes);
        }
        _ => panic!("expected SetupIterm2"),
    }
}

#[test]
fn parses_attach_command_with_and_without_path() {
    let cli = Cli::parse_from(["croft", "attach"]);
    assert!(matches!(
        cli.command,
        Some(CliCommand::Attach {
            path: None,
            solo: false
        })
    ));
    let cli = Cli::parse_from(["croft", "attach", "/work"]);
    match cli.command {
        Some(CliCommand::Attach { path, .. }) => assert_eq!(path, Some(PathBuf::from("/work"))),
        _ => panic!("expected Attach"),
    }
}

#[test]
fn parses_ls_command() {
    let cli = Cli::parse_from(["croft", "ls"]);
    assert!(matches!(cli.command, Some(CliCommand::Ls)));
}

#[test]
fn parses_session_host_probe_serve_and_inner_command() {
    let cli = Cli::parse_from(["croft", "session-host", "--probe"]);
    match cli.command {
        Some(CliCommand::SessionHost { probe, serve, .. }) => {
            assert!(probe);
            assert!(!serve);
        }
        _ => panic!("expected SessionHost"),
    }
    let cli = Cli::parse_from([
        "croft",
        "session-host",
        "--serve",
        "--socket",
        "/x/s.mux.sock",
        "--workspace",
        "/work/repo",
        "--",
        "croft",
        "/work/repo",
    ]);
    match cli.command {
        Some(CliCommand::SessionHost {
            probe,
            serve,
            socket,
            workspace,
            inner,
        }) => {
            assert!(!probe);
            assert!(serve);
            assert_eq!(socket, Some(PathBuf::from("/x/s.mux.sock")));
            assert_eq!(workspace, Some(PathBuf::from("/work/repo")));
            assert_eq!(inner, vec!["croft", "/work/repo"]);
        }
        _ => panic!("expected SessionHost"),
    }
}

#[test]
fn parses_collab_relay_subcommand() {
    let cli = Cli::parse_from(["croft", "collab-relay", "--socket", "/x/s.collab.sock"]);
    match cli.command {
        Some(CliCommand::CollabRelay {
            socket,
            probe,
            ensure,
        }) => {
            assert_eq!(socket, Some(PathBuf::from("/x/s.collab.sock")));
            assert!(!probe);
            assert!(!ensure);
        }
        _ => panic!("expected CollabRelay"),
    }
    // The launch tails probe support and ensure a detached relay.
    let cli = Cli::parse_from(["croft", "collab-relay", "--probe"]);
    assert!(matches!(
        cli.command,
        Some(CliCommand::CollabRelay { probe: true, .. })
    ));
    let cli = Cli::parse_from(["croft", "collab-relay", "--ensure", "--socket", "/x/s.sock"]);
    assert!(matches!(
        cli.command,
        Some(CliCommand::CollabRelay { ensure: true, .. })
    ));
}

#[test]
fn parses_collab_agent_subcommand() {
    let cli = Cli::parse_from([
        "croft",
        "collab-agent",
        "--workspace",
        "/w",
        "--name",
        "bot",
    ]);
    match cli.command {
        Some(CliCommand::CollabAgent { workspace, name }) => {
            assert_eq!(workspace, PathBuf::from("/w"));
            assert_eq!(name.as_deref(), Some("bot"));
        }
        _ => panic!("expected CollabAgent"),
    }
    // The name is optional; the dispatch defaults it.
    let cli = Cli::parse_from(["croft", "collab-agent", "--workspace", "/w"]);
    assert!(matches!(
        cli.command,
        Some(CliCommand::CollabAgent { name: None, .. })
    ));
}

#[test]
fn parses_pair_subcommand() {
    let cli = Cli::parse_from([
        "croft",
        "pair",
        "--workspace",
        "/w",
        "--model",
        "claude-haiku-4-5-20251001",
        "--name",
        "navigator",
        "--repl",
        "fix the loop in src/f.rs",
    ]);
    match cli.command {
        Some(CliCommand::Pair {
            workspace,
            model,
            name,
            task,
            provider: None,
            base_url: None,
            off: false,
            repl: true,
        }) => {
            assert_eq!(workspace, Some(PathBuf::from("/w")));
            assert_eq!(model.as_deref(), Some("claude-haiku-4-5-20251001"));
            assert_eq!(name.as_deref(), Some("navigator"));
            assert_eq!(task.as_deref(), Some("fix the loop in src/f.rs"));
        }
        _ => panic!("expected Pair"),
    }
    // Everything defaults: workspace = cwd, activation only.
    let cli = Cli::parse_from(["croft", "pair"]);
    assert!(matches!(
        cli.command,
        Some(CliCommand::Pair {
            workspace: None,
            model: None,
            name: None,
            task: None,
            provider: None,
            base_url: None,
            off: false,
            repl: false,
        })
    ));
    // --off deactivates; --repl keeps the debug terminal driver.
    let cli = Cli::parse_from(["croft", "pair", "--off"]);
    assert!(matches!(
        cli.command,
        Some(CliCommand::Pair { off: true, .. })
    ));
    let cli = Cli::parse_from(["croft", "pair", "--repl", "do a thing"]);
    assert!(matches!(
        cli.command,
        Some(CliCommand::Pair { repl: true, .. })
    ));
}

/// #1908: only the --repl driver sends a start task, so a task without
/// --repl is a usage error instead of a note that it was ignored, and
/// `croft pair --help`, which hides --repl, does not offer one either.
#[test]
fn pair_task_is_refused_and_hidden_without_repl() {
    let err = Cli::try_parse_from(["croft", "pair", "explain check_loop"])
        .expect_err("a task without --repl is refused");
    assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    let cli = Cli::try_parse_from(["croft", "pair", "--repl", "explain check_loop"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(CliCommand::Pair { repl: true, task: Some(ref t), .. }) if t == "explain check_loop"
    ));
    let mut cmd = <Cli as clap::CommandFactory>::command();
    let help = cmd
        .find_subcommand_mut("pair")
        .expect("pair subcommand")
        .render_long_help()
        .to_string();
    for absent in ["TASK", "first task"] {
        assert!(
            !help.contains(absent),
            "{absent:?} is offered by `croft pair --help`: {help}"
        );
    }
}

/// `--provider ollama --base-url <url>` parses; anything but
/// claude/ollama is refused at parse time.
#[test]
fn parse_pair_provider_ollama_with_base_url() {
    let cli = Cli::parse_from([
        "croft",
        "pair",
        "--provider",
        "ollama",
        "--base-url",
        "http://localhost:11434",
        "--model",
        "qwen3-coder:30b",
    ]);
    match cli.command {
        Some(CliCommand::Pair {
            provider,
            base_url,
            model,
            ..
        }) => {
            assert_eq!(provider.as_deref(), Some("ollama"));
            assert_eq!(base_url.as_deref(), Some("http://localhost:11434"));
            assert_eq!(model.as_deref(), Some("qwen3-coder:30b"));
        }
        _ => panic!("expected Pair"),
    }
    assert!(Cli::try_parse_from(["croft", "pair", "--provider", "openai"]).is_err());
}

/// `--base-url` alone implies the ollama provider, and ollama without an
/// explicit endpoint defaults to the standard local port. Plain claude
/// activations record neither.
#[test]
fn base_url_implies_ollama() {
    let (p, b) = resolve_pair_provider(None, Some(String::from("http://x:1234"))).unwrap();
    assert_eq!(p.as_deref(), Some("ollama"));
    assert_eq!(b.as_deref(), Some("http://x:1234"));

    let (p, b) = resolve_pair_provider(Some(String::from("ollama")), None).unwrap();
    assert_eq!(p.as_deref(), Some("ollama"));
    assert_eq!(b.as_deref(), Some("http://localhost:11434"));

    let (p, b) = resolve_pair_provider(None, None).unwrap();
    assert_eq!(p, None);
    assert_eq!(b, None);
}

/// Validation happens at activation, when the user is watching: a
/// trailing slash built `...//v1/messages` (a 301 that ureq follows by
/// downgrading POST to GET), and a missing scheme only exploded hours
/// later on the first turn.
#[test]
fn base_url_is_normalised_and_validated_at_activation() {
    let (_, b) =
        resolve_pair_provider(None, Some(String::from("http://localhost:11434/"))).unwrap();
    assert_eq!(b.as_deref(), Some("http://localhost:11434"));
    assert!(
        resolve_pair_provider(None, Some(String::from("localhost:11434"))).is_err(),
        "a schemeless endpoint must be refused up front"
    );
    assert!(
        resolve_pair_provider(None, Some(String::from("https://"))).is_err(),
        "an empty host must be refused"
    );
    // Per the WHATWG parser, extra authority slashes are skipped:
    // `https:///v1` is host `v1`, odd but well-formed, and accepted.
    assert!(resolve_pair_provider(None, Some(String::from("https:///v1"))).is_ok());
    assert!(
        resolve_pair_provider(None, Some(String::from("http://local host:1"))).is_err(),
        "whitespace must be refused"
    );
    assert!(
        resolve_pair_provider(None, Some(String::from("http://x:99999999"))).is_err(),
        "a malformed port must be refused"
    );
}

#[test]
fn parses_solo_viewport_flags() {
    // Local: an independent-viewport guest session for a workspace.
    let cli = Cli::parse_from(["croft", "attach", "--solo", "/work"]);
    match cli.command {
        Some(CliCommand::Attach { path, solo }) => {
            assert_eq!(path, Some(PathBuf::from("/work")));
            assert!(solo);
        }
        _ => panic!("expected Attach"),
    }
    let cli = Cli::parse_from(["croft", "attach"]);
    assert!(matches!(
        cli.command,
        Some(CliCommand::Attach { solo: false, .. })
    ));
    // Remote: same opt-in on the SSH launch.
    let cli = Cli::parse_from(["croft", "remote", "reasoner", "/work", "--solo"]);
    assert!(matches!(
        cli.command,
        Some(CliCommand::Remote { solo: true, .. })
    ));
    let cli = Cli::parse_from(["croft", "remote", "reasoner"]);
    assert!(matches!(
        cli.command,
        Some(CliCommand::Remote {
            solo: false,
            build: false,
            ..
        })
    ));
    let cli = Cli::parse_from(["croft", "remote", "reasoner", "--build"]);
    assert!(matches!(
        cli.command,
        Some(CliCommand::Remote { build: true, .. })
    ));
}

#[test]
fn parses_remote_command() {
    let cli = Cli::parse_from(["croft", "remote", "reasoner", "/work"]);
    match cli.command {
        Some(CliCommand::Remote { host, path, .. }) => {
            assert_eq!(host, "reasoner");
            assert_eq!(path, Some(String::from("/work")));
        }
        _ => panic!("expected Remote"),
    }
}
