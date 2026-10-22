//! The message protocol's pure parts: what the recipient reads, and the bound a sender cannot
//! opt out of.

use super::*;

const NOW: Epoch = 1_700_000_000;

/// THE RECIPIENT CANNOT OTHERWISE TELL AN AGENT FROM ITS HUMAN. A message arrives as an
/// ordinary user turn, so the frame is the only thing that says who is asking — and it carries
/// the command that answers, which is what makes a reply need no documentation.
#[test]
fn the_envelope_names_the_sender_and_how_to_reply_and_keeps_the_text_intact() {
    let framed = envelope("start-up", 2, "rerun the Ray GPU pair");
    assert!(framed.contains("message from session start-up"));
    assert!(
        framed.contains("pmtui message --to start-up"),
        "the reply command must name the SENDER: {framed}"
    );
    assert!(framed.contains("hop 2/6"), "{framed}");
    assert!(
        framed.ends_with("rerun the Ray GPU pair"),
        "the message is last and unaltered: {framed}"
    );
    // The frame is a prefix, so a multi-line message keeps its own shape below it.
    let multi = envelope("a", 1, "line one\nline two");
    assert!(multi.ends_with("line one\nline two"), "{multi}");
}

/// A BOUND THE SENDER CANNOT OPT OUT OF. Two driven agents answering each other politely is an
/// unattended token fire, and a hop count carried in the envelope would be reset by an agent
/// that simply omits the flag — so the count lives in the sender's own state and is enforced here.
#[test]
fn hops_count_up_to_the_limit_then_refuse() {
    let mut ledger = None;
    for expected in 1..=HOP_MAX {
        let hops = next_hop(ledger, NOW).expect("under the limit");
        assert_eq!(hops.count, expected);
        assert_eq!(hops.opened_at, NOW, "the window keeps its original start");
        ledger = Some(hops);
    }
    assert!(
        next_hop(ledger, NOW).is_none(),
        "the {HOP_MAX}th send is the last one inside a window"
    );
}

#[test]
fn a_quiet_window_starts_the_count_over() {
    let exhausted = Hops {
        count: HOP_MAX,
        opened_at: NOW,
    };
    // One second early is still the same conversation.
    assert!(next_hop(Some(exhausted), NOW + HOP_WINDOW_S - 1).is_none());
    // Once the window has passed, this is a NEW conversation and not hop 7 of the old one.
    let fresh = next_hop(Some(exhausted), NOW + HOP_WINDOW_S).expect("a new window");
    assert_eq!((fresh.count, fresh.opened_at), (1, NOW + HOP_WINDOW_S));
}
