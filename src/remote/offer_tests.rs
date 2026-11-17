//! The ssh-pane offer's tests, moved out of remote.rs (#2062).

use super::*;
use std::time::{Duration, SystemTime};

#[test]
fn the_offer_policy_respects_every_opt_out() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let none = RefusedHosts::new();
    assert!(offer_allowed("db-1", false, &[], &none, now));
    assert!(
        !offer_allowed("db-1", true, &[], &none, now),
        "the global switch"
    );
    assert!(
        !offer_allowed("db-1", false, &[String::from("DB-1")], &none, now),
        "the per-host list, case-insensitively"
    );
    let refused: RefusedHosts = [(String::from("db-1"), now)].into_iter().collect();
    assert!(
        !offer_allowed("DB-1", false, &[], &refused, now),
        "a learned refusal"
    );
    assert!(offer_allowed(
        "web-2",
        false,
        &[String::from("db-1")],
        &refused,
        now
    ));
    // The TTL applies in memory too: a process open for longer than the
    // window stops honouring a refusal it learned at the start.
    assert!(
        offer_allowed(
            "db-1",
            false,
            &[],
            &refused,
            now + REFUSED_HOST_TTL + Duration::from_secs(1)
        ),
        "an in-memory refusal past the TTL no longer blocks"
    );
}

#[test]
fn a_refusal_is_live_inside_its_window_and_never_from_the_future() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    assert!(refusal_live(now, now));
    assert!(
        refusal_live(now - REFUSED_HOST_TTL, now),
        "the edge is inclusive"
    );
    assert!(!refusal_live(
        now - REFUSED_HOST_TTL - Duration::from_secs(1),
        now
    ));
    assert!(
        !refusal_live(now + Duration::from_secs(1), now),
        "a clock corrected backward must not pin the entry"
    );
}

#[test]
fn pane_samples_give_a_verdict_only_where_there_is_one() {
    // `(shell, fg, name)`: pane 1 at its prompt, pane 2 in vim, pane 3 in
    // an ssh whose argv cannot be read, pane 4 on a known host, pane 5 on
    // a host the config does not know.
    let named = vec![
        (10, 10, String::from("zsh")),
        (20, 21, String::from("vim")),
        (30, 31, String::from("ssh")),
        (40, 41, String::from("ssh")),
        (50, 51, String::from("ssh")),
    ];
    let parses = std::cell::Cell::new(0);
    let targets = || {
        parses.set(parses.get() + 1);
        vec![RemoteTarget {
            alias: String::from("db-1"),
            host_name: Some(String::from("10.0.0.5")),
            user: None,
        }]
    };
    let cmdline = |pid: i32| match pid {
        41 => Some(vec![String::from("ssh"), String::from("db-1")]),
        51 => Some(vec![String::from("ssh"), String::from("elsewhere")]),
        _ => None,
    };
    let samples = ssh_pane_samples(&named, targets, cmdline);
    assert_eq!(
        samples,
        vec![
            (10, None),
            (20, None),
            (40, Some(String::from("db-1"))),
            (50, None),
        ],
        "the unreadable ssh (pane 3) gets no verdict at all"
    );
    assert_eq!(parses.get(), 1, "the config is parsed once per pass");

    let parses = std::cell::Cell::new(0);
    let quiet = vec![(10, 10, String::from("zsh")), (20, 21, String::from("vim"))];
    let samples = ssh_pane_samples(
        &quiet,
        || {
            parses.set(parses.get() + 1);
            Vec::new()
        },
        |_| None,
    );
    assert_eq!(samples, vec![(10, None), (20, None)]);
    assert_eq!(parses.get(), 0, "and not at all when no pane is ssh");
}

#[test]
fn refused_hosts_round_trip_and_expire() {
    let dir = tempfile::tempdir().unwrap();
    let path = refused_hosts_path(dir.path());
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    assert!(
        load_refused_hosts(&path, now).is_empty(),
        "no file, no memory"
    );
    remember_refused_host(&path, "Jump", now).unwrap();
    remember_refused_host(
        &path,
        "old-box",
        now - REFUSED_HOST_TTL - Duration::from_secs(1),
    )
    .unwrap();
    remember_refused_host(&path, "ahead", now + Duration::from_secs(60)).unwrap();
    let loaded = load_refused_hosts(&path, now);
    assert!(
        loaded.contains_key("jump"),
        "stored lower-cased: {loaded:?}"
    );
    assert_eq!(loaded["jump"], now, "and its time comes back with it");
    assert!(
        !loaded.contains_key("old-box"),
        "an entry past the TTL is forgotten"
    );
    assert!(
        !loaded.contains_key("ahead"),
        "an entry from the future (clock moved back) is forgotten too"
    );
    std::fs::write(&path, "not json").unwrap();
    assert!(
        load_refused_hosts(&path, now).is_empty(),
        "garbage reads as empty"
    );
}

#[test]
fn only_ssh_itself_is_an_ssh_program() {
    assert!(is_ssh_program("ssh"));
    assert!(is_ssh_program("/usr/bin/ssh"), "a path is tolerated");
    assert!(
        !is_ssh_program("ssh-agent"),
        "whole-name match, as SSH_PROGRAMS promises"
    );
    assert!(!is_ssh_program("sshfs"));
    assert!(
        !is_ssh_program("mosh"),
        "long-option programs are refused (see #445)"
    );
    assert!(!is_ssh_program(""));
}

#[test]
fn a_success_forgets_a_remembered_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let path = refused_hosts_path(dir.path());
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    forget_refused_host(&path, "nobody").unwrap();
    assert!(!path.exists(), "forgetting with no file creates nothing");
    remember_refused_host(&path, "db-1", now).unwrap();
    remember_refused_host(&path, "jump", now).unwrap();
    forget_refused_host(&path, "DB-1").unwrap();
    let left = load_refused_hosts(&path, now);
    assert!(
        !left.contains_key("db-1") && left.contains_key("jump"),
        "{left:?}"
    );
}

#[test]
fn the_offer_host_is_the_config_alias_or_nothing() {
    let targets = vec![RemoteTarget {
        alias: String::from("db-1"),
        host_name: Some(String::from("10.0.0.5")),
        user: None,
    }];
    assert_eq!(
        ssh_offer_host(&["ssh", "-p", "2222", "db-1"], &targets).as_deref(),
        Some("db-1")
    );
    assert_eq!(ssh_offer_host(&["ssh", "unknown-box"], &targets), None);
    assert_eq!(ssh_offer_host(&["vim", "db-1"], &targets), None);
}
