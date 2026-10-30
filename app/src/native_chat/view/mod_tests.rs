use super::*;

#[test]
fn cards_dock_only_in_a_wide_enough_pane() {
    let sizes = Sizes::default();
    let min_width = min_width_for_docked_cards(&sizes);
    assert!(min_width > layout::MIN_DOCKED_CONVERSATION_WIDTH + docked_cards_width(&sizes));
    // A laptop window with the Threads sidebar open leaves the chat about this much.
    assert!(min_width <= 1000.);

    assert!(has_room_for_cards(min_width, &sizes));
    assert!(has_room_for_cards(min_width + 400., &sizes));
    assert!(!has_room_for_cards(min_width - 1., &sizes));
    assert!(!has_room_for_cards(0., &sizes));
}

#[test]
fn docked_conversation_stays_centered_while_there_is_room() {
    let sizes = Sizes::default();
    let cards = docked_cards_width(&sizes);
    let conversation = conversation_max_width(&sizes);

    assert_eq!(
        docked_left_inset(min_width_for_docked_cards(&sizes), &sizes),
        0.
    );
    assert_eq!(docked_left_inset(conversation + cards, &sizes), 0.);
    assert_eq!(docked_left_inset(conversation + cards + 50., &sizes), 50.);
    assert_eq!(docked_left_inset(conversation + 2. * cards, &sizes), cards);
    assert_eq!(docked_left_inset(conversation + 4. * cards, &sizes), cards);
}
