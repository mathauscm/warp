use std::collections::HashMap;

use warpui::EntityId;

use super::{chat_tab_to_reuse, unused_chat_tabs_to_close};
use crate::native_chat::registry::ChatActivity;

fn id(value: usize) -> EntityId {
    EntityId::from_usize(value)
}

#[test]
fn reuses_the_most_recent_chat_tab_that_can_be_replaced() {
    let chat_tabs = HashMap::from([
        (id(1), ChatActivity::Idle),
        (id(2), ChatActivity::Idle),
        (id(3), ChatActivity::Unused),
    ]);
    assert_eq!(
        chat_tab_to_reuse(&[id(2), id(1), id(3)], &chat_tabs),
        Some(id(2))
    );
}

#[test]
fn skips_busy_and_drafting_chats_and_other_tabs() {
    let chat_tabs = HashMap::from([
        (id(1), ChatActivity::Busy),
        (id(2), ChatActivity::Drafting),
        (id(4), ChatActivity::Idle),
    ]);
    // Tab 3 isn't a lone chat (a terminal, or a chat in a split).
    assert_eq!(
        chat_tab_to_reuse(&[id(1), id(3), id(2), id(4)], &chat_tabs),
        Some(id(4))
    );
}

#[test]
fn opens_a_new_tab_when_every_chat_is_busy() {
    let chat_tabs = HashMap::from([(id(1), ChatActivity::Busy), (id(2), ChatActivity::Drafting)]);
    assert_eq!(chat_tab_to_reuse(&[id(1), id(2), id(3)], &chat_tabs), None);
}

#[test]
fn closes_unused_chats_except_the_active_tab() {
    let chat_tabs = HashMap::from([
        (id(1), ChatActivity::Unused),
        (id(2), ChatActivity::Unused),
        (id(3), ChatActivity::Idle),
        (id(4), ChatActivity::Drafting),
        (id(5), ChatActivity::Busy),
    ]);
    assert_eq!(unused_chat_tabs_to_close(id(1), &chat_tabs), vec![id(2)]);
}
