//! The tests of the set of debug sessions in flight, moved out of session.rs (#2062).

use super::DebugSessions;

/// A configuration launch REPLACES; a compound member JOINS. Getting that
/// backwards is the bug #310 exists to prevent - the second member of a
/// compound would kill the first, and the user would see one session
/// where they asked for two.
#[test]
fn a_replace_clears_the_set_while_a_push_joins_it() {
    let mut set: DebugSessions<&str> = DebugSessions::default();
    assert!(set.is_empty(), "nothing is being debugged to start with");

    set.replace_with("server", "s1");
    set.push("client", "c1");
    assert_eq!(set.len(), 2, "the member joined rather than replacing");
    assert_eq!(set.names(), vec!["server", "client"], "in launch order");

    // A configuration launch after a compound is still a full stop: the
    // user picked one thing, so one thing runs.
    set.replace_with("alone", "a1");
    assert_eq!(set.names(), vec!["alone"]);
}

/// The UI reads ONE session. Whichever it is must exist, and must survive
/// the operations that change the set, or the call stack and variables
/// describe a session that is not there.
#[test]
fn focus_always_names_a_session_that_exists() {
    let mut set: DebugSessions<&str> = DebugSessions::default();
    assert_eq!(set.focused(), None, "an empty set focuses nothing");
    assert_eq!(set.focused_name(), None);

    set.replace_with("first", "f");
    set.push("second", "s");
    assert_eq!(
        set.focused_name(),
        Some("second"),
        "a push focuses what it just started"
    );

    assert!(set.focus(0), "an existing index is focusable");
    assert_eq!(set.focused(), Some(&"f"));
    assert!(
        !set.focus(9),
        "a missing one is refused rather than dangling"
    );
    assert_eq!(
        set.focused_name(),
        Some("first"),
        "and the refusal leaves the previous focus untouched"
    );

    set.clear();
    assert!(set.is_empty() && set.focused().is_none());
    // The index went back with it: a stale focus into an empty set is how
    // a cleared debugger still paints a call stack.
    set.replace_with("fresh", "x");
    assert_eq!(set.focused_name(), Some("fresh"));
}

/// One member ending must retire THAT member, not the set. Dropping the
/// set reaches its siblings as a kill rather than the graceful
/// disconnect, and turns every compound into stop-all.
#[test]
fn removing_one_session_leaves_the_others_and_keeps_the_focus_valid() {
    let mut set: DebugSessions<&str> = DebugSessions::default();
    set.replace_with("server", "s");
    set.push("client", "c");
    set.push("worker", "w");

    set.focus(2);
    // The one being watched ends: focus steps back rather than dangling.
    assert_eq!(set.remove(2).map(|s| s.name), Some(String::from("worker")));
    assert_eq!(set.names(), vec!["server", "client"]);
    assert_eq!(set.focused_name(), Some("client"));

    // One BEFORE the focused one ends: the user keeps watching the same
    // session, which is now at a lower index.
    set.focus(1);
    assert_eq!(set.remove(0).map(|s| s.name), Some(String::from("server")));
    assert_eq!(set.focused_name(), Some("client"), "still the same session");

    assert!(set.remove(5).is_none(), "a missing index removes nothing");

    // The MIDDLE case, which neither arm above reaches: the focused
    // session has a live sibling on each side. Focus is positional, so
    // the user lands on the one that moved up into the slot rather than
    // being thrown back to the start of the set.
    let mut three: DebugSessions<&str> = DebugSessions::default();
    three.replace_with("a", "1");
    three.push("b", "2");
    three.push("c", "3");
    three.focus(1);
    assert_eq!(three.remove(1).map(|s| s.name), Some(String::from("b")));
    assert_eq!(three.names(), vec!["a", "c"]);
    assert_eq!(
        three.focused_name(),
        Some("c"),
        "the next live session, not the first"
    );

    assert_eq!(set.len(), 1);

    set.remove(0);
    assert!(set.is_empty() && set.focused().is_none());
}

/// Every session is reachable WITH its position, which is what lets the
/// poll drive all the adapters while only one of them drives the UI.
#[test]
fn every_session_is_reachable_with_its_index() {
    let mut set: DebugSessions<u32> = DebugSessions::default();
    set.replace_with("a", 10);
    set.push("b", 20);
    set.push("c", 30);
    let seen: Vec<(usize, u32)> = set
        .iter_named_mut_indexed()
        .map(|(i, s)| (i, s.session))
        .collect();
    assert_eq!(
        seen,
        vec![(0, 10), (1, 20), (2, 30)],
        "all three with their positions - a poll that visited only the \
             focused one would leave the rest at the gate, started and dead"
    );
}

/// Stop means stop ALL. `iter_mut` is what the stop walks, so it has to
/// reach every session rather than the focused one.
#[test]
fn every_session_is_reachable_for_a_stop() {
    let mut set: DebugSessions<u32> = DebugSessions::default();
    set.replace_with("a", 1);
    set.push("b", 2);
    set.push("c", 3);
    set.focus(1);

    let seen: Vec<u32> = set.iter_mut().map(|s| *s).collect();
    assert_eq!(
        seen,
        vec![1, 2, 3],
        "all three, not just the focused one - a stop that misses a \
             session orphans a live process"
    );
}
