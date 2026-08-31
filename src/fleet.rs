//! Running one command across several hosts and comparing what came back
//! (#363).
//!
//! Broadcast typing already covers "type once"; this is the other half —
//! laying the outputs side by side and saying what differs. The comparison
//! is the whole value, because reading ten near-identical `uname -r` outputs
//! by eye is exactly the task a person is worst at.
//!
//! # What "the reference" means
//!
//! Diff mode needs something to diff against, and the useful default is the
//! output MOST hosts produced — the fleet's normal, so the tiles that light
//! up are the exceptions. Not the first host: that makes the answer depend
//! on the order hosts happen to be listed in, so the same fleet in a
//! different order would highlight a different set.
//!
//! A tie is broken by picking neither. With 2-and-2 there is no majority and
//! no honest "normal" to compare against, so saying so beats choosing one
//! arbitrarily and painting half the fleet as deviant.

use std::collections::BTreeMap;
use std::time::Duration;

/// What one host returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostResult {
    pub host: String,
    /// Combined stdout+stderr, as the tile shows it.
    pub output: String,
    /// `None` when the run timed out or ssh never returned a status.
    pub exit: Option<i32>,
}

impl HostResult {
    /// Whether this host answered successfully.
    ///
    /// A non-zero exit is a FAILURE for fleet purposes even though its
    /// output may be perfectly good: the point of the summary is to say
    /// which hosts to look at, and one that errored is one to look at.
    pub fn ok(&self) -> bool {
        self.exit == Some(0)
    }
}

/// How a fleet run turned out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetSummary {
    /// Hosts whose output matches the reference.
    pub identical: usize,
    /// Hosts that succeeded but returned something else.
    pub differ: usize,
    /// Hosts that failed or timed out.
    pub failed: usize,
}

impl FleetSummary {
    /// The header line: `7 identical · 2 differ · 1 failed`.
    ///
    /// Zero counts are omitted rather than shown as `0 failed`, because a
    /// clean run should read as clean at a glance instead of making the
    /// reader parse three numbers to discover two are zero.
    pub fn line(&self) -> String {
        let mut parts = Vec::new();
        if self.identical > 0 {
            parts.push(format!("{} identical", self.identical));
        }
        if self.differ > 0 {
            parts.push(format!("{} differ", self.differ));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        if parts.is_empty() {
            return String::from("no hosts");
        }
        parts.join(" \u{b7} ")
    }
}

/// The output most successful hosts returned, or `None` when no single
/// output leads.
///
/// A PLURALITY, not a majority: with `x,x,y,z` the winner has 2 of 4, and
/// that is the right answer — the two odd hosts are still the exceptions.
/// Only an exact TIE at the top yields `None`.
///
/// Only SUCCESSFUL hosts vote. A failing host's output is usually an error
/// message, and letting three identical `Permission denied` results become
/// the reference would paint every working host as the deviant one.
pub fn reference_output(results: &[HostResult]) -> Option<&str> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in results.iter().filter(|r| r.ok()) {
        *counts.entry(r.output.as_str()).or_default() += 1;
    }
    let best = counts.values().copied().max()?;
    // A tie means no majority: with 2-and-2 there is no honest "normal", and
    // choosing one would paint half the fleet as deviant on the strength of
    // nothing. `BTreeMap` order would make that choice deterministic, which
    // is worse — a stable wrong answer looks like a considered one.
    let winners = counts.values().filter(|&&c| c == best).count();
    if winners > 1 {
        return None;
    }
    counts
        .into_iter()
        .find(|&(_, c)| c == best)
        .map(|(out, _)| out)
}

/// Count the run against `reference`.
pub fn summarise(results: &[HostResult], reference: Option<&str>) -> FleetSummary {
    let mut s = FleetSummary {
        identical: 0,
        differ: 0,
        failed: 0,
    };
    for r in results {
        if !r.ok() {
            s.failed += 1;
        } else if Some(r.output.as_str()) == reference {
            s.identical += 1;
        } else {
            s.differ += 1;
        }
    }
    s
}

/// The ssh argv for running `command` on `host` non-interactively.
///
/// `-n` because stdin belongs to croft, not to the remote command: without
/// it a fleet run reads the terminal the user is typing into, and the hosts
/// race each other for the keystrokes. Same reason `remote.rs` passes it.
///
/// `BatchMode=yes` so a host that wants a password FAILS rather than hanging
/// on a prompt nobody can see — a timeout the user waits out is worse than
/// an error they can act on.
///
/// `StrictHostKeyChecking=yes` — a host with no `known_hosts` entry FAILS
/// rather than being trusted on sight.
///
/// `accept-new` was the first choice and is wrong here specifically because
/// this is a fleet. A single interactive `ssh` shows the user a fingerprint
/// and asks; a fleet run shows nobody anything, so `accept-new` would
/// silently trust a first-contact key on every unknown host at once — and an
/// attacker impersonating one of them receives the command the user typed.
/// Failing costs one `ssh host` to establish the key, done deliberately and
/// with the fingerprint on screen, which is where that decision belongs.
pub fn fleet_ssh_args(host: &str, command: &str) -> Vec<String> {
    vec![
        String::from("-n"),
        String::from("-o"),
        String::from("BatchMode=yes"),
        String::from("-o"),
        String::from("StrictHostKeyChecking=yes"),
        String::from(host),
        String::from("--"),
        String::from(command),
    ]
}

/// Split a fleet request into the hosts it names and the command to run.
///
/// The syntax is `host,host,host: command` — the fleet FIRST, because
/// choosing who to run on is the decision, and a user typing left to right
/// should have to make it before writing anything that could execute.
///
/// `None` when no host list is given. That refusal is the point: without it
/// "Fleet Run" means "run on every host in your ~/.ssh/config", which on an
/// ordinary machine is every remote you have ever configured — a live
/// production box beside a `github.com` entry that is not a shell host at
/// all. A prompt asking you to confirm a list you cannot see is not consent,
/// so the fleet has to be named rather than defaulted.
///
/// `*` is the explicit way to say "all of them", so the broadcast is still
/// available to someone who means it.
pub fn parse_request<'a>(input: &str, known: &'a [String]) -> Option<(Vec<&'a String>, String)> {
    let (spec, command) = input.split_once(':')?;
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    let spec = spec.trim();
    if spec == "*" {
        return Some((known.iter().collect(), command.to_string()));
    }
    let mut hosts = Vec::new();
    for name in spec.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        // Only hosts croft actually knows: a typo must not become an ssh
        // attempt against a hostname the user never configured.
        let found = known.iter().find(|k| k.eq_ignore_ascii_case(name))?;
        if !hosts.contains(&found) {
            hosts.push(found);
        }
    }
    (!hosts.is_empty()).then_some((hosts, command.to_string()))
}

/// Run `command` on every host in parallel, one thread each.
///
/// Parallel because the whole point is comparing a fleet: serially, ten
/// hosts at a two-second timeout is twenty seconds of staring, and one
/// unreachable host holds every other result behind it. The issue's own
/// criterion says a timing-out host must not delay the others.
///
/// The timeout is enforced by ssh's `ConnectTimeout` plus a wall-clock
/// deadline on the join, so a host that connects and then hangs is bounded
/// too — `ConnectTimeout` alone only covers the handshake.
pub fn run_on_hosts(hosts: &[String], command: &str, timeout: Duration) -> Vec<HostResult> {
    let (tx, rx) = std::sync::mpsc::channel();
    for (idx, host) in hosts.iter().enumerate() {
        let tx = tx.clone();
        let host = host.clone();
        let command = command.to_string();
        std::thread::spawn(move || {
            // Keyed by REQUEST INDEX, not host name. Two entries naming the
            // same host would otherwise collide in the collection: the second
            // reply overwrites the first, one slot consumes it, and the other
            // is filled in as a timeout — so a host that answered fine is
            // reported red and the surviving result is whichever thread was
            // SLOWER. The index makes duplicates independent.
            let _ = tx.send((idx, run_one(&host, &command, timeout)));
        });
    }
    drop(tx);

    let deadline = std::time::Instant::now() + timeout + Duration::from_secs(2);
    let mut got: Vec<Option<HostResult>> = (0..hosts.len()).map(|_| None).collect();
    let mut filled = 0usize;
    while filled < hosts.len() {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok((idx, r)) => {
                if got.get(idx).is_some_and(Option::is_none) {
                    got[idx] = Some(r);
                    filled += 1;
                }
            }
            Err(_) => break,
        }
    }
    // A host that never reported still gets a tile: a fleet view that
    // silently drops one is worse than one showing it red, because the user
    // counts tiles to know the run was complete.
    got.into_iter()
        .zip(hosts)
        .map(|(r, h)| {
            r.unwrap_or_else(|| HostResult {
                host: h.clone(),
                output: String::from("timed out"),
                exit: None,
            })
        })
        .collect()
}

/// One host's run, killed if it outlives `timeout`.
///
/// The kill is the point rather than tidiness. `ConnectTimeout` bounds the
/// HANDSHAKE only — measured: a `sleep 30` under a 3-second connect timeout
/// ran the full thirty seconds — so a host that connects and then hangs
/// leaves an `ssh` child running after the deadline has already reported it
/// as timed out. Every subsequent run leaks another, and a command the user
/// gave up on can still complete minutes later.
fn run_one(host: &str, command: &str, timeout: Duration) -> HostResult {
    let secs = timeout.as_secs().max(1);
    let mut args = vec![String::from("-o"), format!("ConnectTimeout={secs}")];
    args.extend(fleet_ssh_args(host, command));
    let spawned = std::process::Command::new("ssh")
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            return HostResult {
                host: host.to_string(),
                output: format!("could not run ssh: {e}"),
                exit: None,
            };
        }
    };
    // Poll rather than block, so the child can be killed when its time is
    // up. `wait_with_output` would consume the child and leave no handle to
    // kill, which is how the abandoned processes accumulated.
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                // Reaped, so the process does not linger as a zombie.
                let _ = child.wait();
                return HostResult {
                    host: host.to_string(),
                    output: String::from("timed out"),
                    exit: None,
                };
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => {
                return HostResult {
                    host: host.to_string(),
                    output: format!("could not wait on ssh: {e}"),
                    exit: None,
                };
            }
        }
    }
    let out = child.wait_with_output();
    match out {
        Ok(o) => {
            // stdout AND stderr: a command that failed usually said why on
            // stderr, and a tile showing an empty box for a failure tells
            // the user nothing they can act on.
            let mut text = String::from_utf8_lossy(&o.stdout).trim_end().to_string();
            let err = String::from_utf8_lossy(&o.stderr);
            let err = err.trim_end();
            if !err.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(err);
            }
            HostResult {
                host: host.to_string(),
                output: text,
                exit: o.status.code(),
            }
        }
        Err(e) => HostResult {
            host: host.to_string(),
            output: format!("could not run ssh: {e}"),
            exit: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(host: &str, out: &str, exit: Option<i32>) -> HostResult {
        HostResult {
            host: String::from(host),
            output: String::from(out),
            exit,
        }
    }

    /// The reference is the majority output, not the first host's.
    ///
    /// Asserted with the odd one out FIRST in the list, because "the first
    /// host" and "the majority" agree in most orderings — a fixture where
    /// they coincide cannot tell which rule is implemented.
    #[test]
    fn the_reference_is_the_majority_not_the_first_host() {
        let results = vec![
            r("odd", "6.1.0", Some(0)),
            r("a", "5.15.0", Some(0)),
            r("b", "5.15.0", Some(0)),
            r("c", "5.15.0", Some(0)),
        ];
        assert_eq!(
            reference_output(&results),
            Some("5.15.0"),
            "the fleet's normal, not whichever host was listed first"
        );

        let s = summarise(&results, reference_output(&results));
        assert_eq!(
            (s.identical, s.differ, s.failed),
            (3, 1, 0),
            "the odd host is the one that differs"
        );
        assert_eq!(s.line(), "3 identical \u{b7} 1 differ");
    }

    /// A failing host does not get a vote, and is never the reference.
    ///
    /// Its output is usually an error message, and letting three identical
    /// `Permission denied` results become the normal would paint every
    /// working host as the deviant one — the exact inversion of what the
    /// summary is for.
    #[test]
    fn a_failing_host_does_not_define_the_normal() {
        let results = vec![
            r("bad1", "Permission denied", Some(255)),
            r("bad2", "Permission denied", Some(255)),
            r("bad3", "Permission denied", None),
            r("good", "5.15.0", Some(0)),
        ];
        assert_eq!(
            reference_output(&results),
            Some("5.15.0"),
            "only successful hosts vote"
        );
        let s = summarise(&results, reference_output(&results));
        assert_eq!((s.identical, s.differ, s.failed), (1, 0, 3));
        assert_eq!(s.line(), "1 identical \u{b7} 3 failed");
    }

    /// An even split has no normal, and says so rather than picking one.
    ///
    /// Choosing arbitrarily would paint half the fleet as deviant on the
    /// strength of nothing — and would do it DETERMINISTICALLY, which is
    /// worse, because a stable wrong answer reads as a considered one.
    #[test]
    fn an_even_split_has_no_reference() {
        let results = vec![
            r("a", "x", Some(0)),
            r("b", "x", Some(0)),
            r("c", "y", Some(0)),
            r("d", "y", Some(0)),
        ];
        assert_eq!(reference_output(&results), None, "2-and-2 has no majority");

        // With no reference every successful host counts as differing:
        // nothing has been declared normal, so nothing matches it.
        let s = summarise(&results, None);
        assert_eq!((s.identical, s.differ, s.failed), (0, 4, 0));
        assert_eq!(s.line(), "4 differ");
    }

    /// Nothing to run, and everything failing, both read honestly.
    #[test]
    fn an_empty_or_wholly_failed_run_says_so() {
        assert_eq!(reference_output(&[]), None);
        let empty = summarise(&[], None);
        assert_eq!(empty.line(), "no hosts", "zero hosts is not '0 identical'");

        let all_bad = vec![r("a", "boom", Some(1)), r("b", "boom", None)];
        assert_eq!(
            reference_output(&all_bad),
            None,
            "no successful host means no normal"
        );
        assert_eq!(summarise(&all_bad, None).line(), "2 failed");
    }

    /// Every host gets a tile, including one that never reports.
    ///
    /// The issue's criterion is that a timing-out host shows red WITHOUT
    /// delaying the others, and the half that is easy to get wrong is the
    /// tile count: a fleet view that silently drops a host is worse than one
    /// showing it failed, because the user counts tiles to know the run was
    /// complete.
    ///
    /// Driven with unreachable hosts and a short timeout so it stays fast:
    /// what is under test is the collection and fill-in, not ssh.
    #[test]
    fn a_host_that_never_reports_still_gets_a_tile() {
        let hosts: Vec<String> = ["no-such-host-a.invalid", "no-such-host-b.invalid"]
            .iter()
            .map(|s| String::from(*s))
            .collect();
        let started = std::time::Instant::now();
        let out = run_on_hosts(&hosts, "true", Duration::from_secs(1));

        assert_eq!(out.len(), hosts.len(), "one tile per host, always");
        // Order follows the HOSTS list, not completion order, so the tiles
        // do not rearrange themselves between runs.
        assert_eq!(out[0].host, hosts[0]);
        assert_eq!(out[1].host, hosts[1]);
        for r in &out {
            assert!(!r.ok(), "an unreachable host must not read as success");
            assert!(!r.output.is_empty(), "a failed tile must say something");
        }
        // Parallel: two hosts at a 1s timeout must not take two timeouts.
        assert!(
            started.elapsed() < Duration::from_secs(12),
            "hosts ran serially: {:?}",
            started.elapsed()
        );

        // A DUPLICATE host exercises the fill-in directly. Both threads
        // report under the same key, so the map holds one entry for two
        // requested hosts — and the result must still be two tiles, in the
        // requested order. Without the fill-in this returns one tile and a
        // host silently vanishes from the fleet view.
        let dup = vec![
            String::from("no-such-host-a.invalid"),
            String::from("no-such-host-a.invalid"),
        ];
        let out = run_on_hosts(&dup, "true", Duration::from_secs(1));
        assert_eq!(
            out.len(),
            2,
            "a host requested twice must yield two tiles, not one"
        );
        assert!(out.iter().all(|r| r.host == dup[0]));
        // And BOTH carry a real answer. Keying the collection by host name
        // made the second reply overwrite the first, so one slot consumed the
        // single entry and the other was filled in as a timeout — a host that
        // answered reported red, with the surviving result whichever thread
        // was slower. Asserting only the count cannot see that.
        assert!(
            out.iter().all(|r| r.output != "timed out"),
            "a duplicate lost its real result: {out:?}"
        );
    }

    /// The fleet must be NAMED; there is no default of "everything".
    ///
    /// Without this, "Fleet Run" means "run on every host in your
    /// ~/.ssh/config" — on an ordinary machine every remote ever configured,
    /// a live production box beside a `github.com` entry that is not a shell
    /// host at all. And a confirmation saying "run on 5 hosts?" asks the
    /// user to approve a list they cannot see, which is not consent.
    #[test]
    fn a_fleet_must_be_named_rather_than_defaulted() {
        let known: Vec<String> = ["web-1", "web-2", "db-1"]
            .iter()
            .map(|s| String::from(*s))
            .collect();
        let p = |input: &str| {
            parse_request(input, &known)
                .map(|(h, c)| (h.iter().map(|s| s.as_str()).collect::<Vec<_>>(), c))
        };

        assert_eq!(
            p("web-1: uname -r"),
            Some((vec!["web-1"], String::from("uname -r")))
        );
        assert_eq!(
            p("web-1,db-1: uptime"),
            Some((vec!["web-1", "db-1"], String::from("uptime")))
        );
        // Whitespace and case are the user's, not the parser's business.
        assert_eq!(
            p("  WEB-2 , db-1 :  df -h  "),
            Some((vec!["web-2", "db-1"], String::from("df -h")))
        );
        // `*` is how you say "all of them" ON PURPOSE, so the broadcast is
        // still available to someone who means it.
        assert_eq!(
            p("*: uptime").map(|(h, _)| h.len()),
            Some(3),
            "a deliberate broadcast is still possible"
        );
        // A host repeated is one host: two threads racing on one name would
        // otherwise collide in the collection.
        assert_eq!(p("db-1,db-1: uptime").map(|(h, _)| h.len()), Some(1));

        for bad in [
            "uname -r",             // no fleet named at all
            ": uname -r",           // an empty fleet is not "everything"
            "web-1:",               // no command
            "web-1:   ",            // whitespace is no command
            "nosuchhost: uptime",   // a typo must not become an ssh attempt
            "web-1,nosuchhost: up", // one bad name refuses the whole run
            "",
        ] {
            assert_eq!(p(bad), None, "must refuse: {bad:?}");
        }
    }

    /// The ssh invocation cannot steal the user's keyboard or hang on a
    /// password prompt.
    #[test]
    fn the_fleet_ssh_invocation_is_non_interactive() {
        let args = fleet_ssh_args("db-1", "uname -r");
        assert!(
            args.contains(&String::from("-n")),
            "without -n the remote command reads the terminal croft is using"
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["-o", "BatchMode=yes"].map(String::from)),
            "a host wanting a password must fail, not hang: {args:?}"
        );
        // An UNKNOWN host must fail rather than be trusted on sight. A single
        // interactive ssh shows a fingerprint and asks; a fleet run shows
        // nobody anything, so `accept-new` would silently trust a first
        // contact on every unknown host at once.
        assert!(
            args.windows(2)
                .any(|w| w == ["-o", "StrictHostKeyChecking=yes"].map(String::from)),
            "a fleet must not auto-trust first-contact keys: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a.contains("accept-new")),
            "accept-new trusts an unseen key with nobody watching: {args:?}"
        );
        // The command is one argument after `--`, so a command containing
        // spaces or a host-looking word cannot be read as another option.
        let tail: Vec<&String> = args.iter().skip_while(|a| *a != "--").collect();
        assert_eq!(tail.len(), 2, "exactly one command argument: {args:?}");
        assert_eq!(tail[1], "uname -r");
    }
}
