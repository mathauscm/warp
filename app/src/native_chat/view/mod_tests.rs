use super::*;

#[test]
fn cards_dock_only_in_a_wide_single_pane() {
    let sizes = Sizes::default();
    let min_width = min_width_for_docked_cards(&sizes);
    assert!(min_width > sizes.column_max_width + 2. * sizes.side_panel_width);

    assert!(has_room_for_cards(false, min_width, &sizes));
    assert!(has_room_for_cards(false, min_width + 400., &sizes));
    assert!(!has_room_for_cards(false, min_width - 1., &sizes));
    assert!(!has_room_for_cards(true, min_width + 400., &sizes));
    assert!(!has_room_for_cards(false, 0., &sizes));
}
