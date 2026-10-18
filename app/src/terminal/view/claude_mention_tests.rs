use super::quote;

#[test]
fn quotes_each_line_and_leaves_room_for_the_question() {
    assert_eq!(quote("fn main() {}"), "> fn main() {}\n\n");
    assert_eq!(quote("a\nb"), "> a\n> b\n\n");
}

#[test]
fn keeps_blank_lines_inside_the_quote_and_drops_the_outer_ones() {
    assert_eq!(quote("\na\n\nb  \n\n"), "> a\n>\n> b\n\n");
}
