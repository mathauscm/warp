use super::{ChatActivity, NativeChatStatus};

#[test]
fn a_chat_that_never_sent_anything_is_unused() {
    assert_eq!(
        ChatActivity::new(false, false, NativeChatStatus::Idle),
        ChatActivity::Unused
    );
}

#[test]
fn typed_text_keeps_a_chat_from_being_replaced() {
    for has_transcript in [false, true] {
        let activity = ChatActivity::new(has_transcript, true, NativeChatStatus::Idle);
        assert_eq!(activity, ChatActivity::Drafting);
        assert!(!activity.can_be_replaced());
    }
}

#[test]
fn a_working_or_waiting_chat_is_busy() {
    for status in [NativeChatStatus::Working, NativeChatStatus::AwaitingUser] {
        for (has_transcript, has_draft) in [(false, false), (true, false), (true, true)] {
            let activity = ChatActivity::new(has_transcript, has_draft, status);
            assert_eq!(activity, ChatActivity::Busy);
            assert!(!activity.can_be_replaced());
        }
    }
}

#[test]
fn saved_or_unused_chats_can_show_another_conversation() {
    assert!(ChatActivity::new(true, false, NativeChatStatus::Idle).can_be_replaced());
    assert!(ChatActivity::new(false, false, NativeChatStatus::Idle).can_be_replaced());
}
