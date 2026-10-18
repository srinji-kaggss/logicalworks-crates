//! The shipped domains' observed values, read through their accessors, run as tests (#526).
//!
//! Each value used to carry a doctest that named the crate by path. The four are tests here, so
//! the claim the doc prose makes (the fields are private and every value is built whole) runs
//! under nextest with the rest of the suite.

use lgwks_bot::domain::chat::ChatMessage;
use lgwks_bot::domain::data::DataState;
use lgwks_bot::domain::net::NetState;
use lgwks_bot::domain::notify::Message;

#[test]
fn a_chat_message_is_read_through_its_accessors() {
    let observed = ChatMessage::new("#deploys", "ci-bot", "rollout finished", "42");
    assert!(observed.contains("finished"));
    assert_eq!(observed.channel(), "#deploys");
    assert_eq!(observed.sender(), "ci-bot");
    assert_eq!(observed.text(), "rollout finished");
    assert_eq!(observed.ts(), "42");
}

#[test]
fn a_data_reading_pairs_the_flag_with_the_bytes_it_describes() {
    let reading = DataState::new(false, "{\"ok\":true}");
    assert!(!reading.changed());
    assert_eq!(reading.raw(), "{\"ok\":true}");
}

#[test]
fn a_network_reading_is_one_probes_status_reachability_and_preview() {
    let answered = NetState::new(200, true, "ok");
    assert!(answered.reachable());
    assert_eq!(answered.status_code(), 200);
    assert_eq!(answered.body(), "ok");
}

#[test]
fn a_notification_message_carries_the_text_it_will_send() {
    let outgoing = Message::new("rollout 42 finished");
    assert_eq!(outgoing.text(), "rollout 42 finished");
}
