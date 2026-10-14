use super::*;

fn typed(s: &str) -> FilterUi {
    let mut u = FilterUi::default();
    for c in s.chars() {
        u.edit(Edit::Char(c));
    }
    u
}

fn mv(to: Motion) -> Edit {
    Edit::Move { to, extend: false }
}

fn ext(to: Motion) -> Edit {
    Edit::Move { to, extend: true }
}

#[test]
fn typing_inserts_at_the_caret_and_reports_a_change() {
    let mut u = typed("ac");
    u.edit(mv(Motion::Left));
    assert!(u.edit(Edit::Char('b')));
    assert_eq!((u.text.as_str(), u.caret), ("abc", 2));
}

#[test]
fn backspace_and_delete_remove_whole_characters() {
    let mut u = typed("a\u{e9}\u{1F600}");
    assert!(u.edit(Edit::Backspace));
    assert_eq!(u.text, "a\u{e9}");
    u.edit(mv(Motion::Left)); // before the e-acute
    assert!(u.edit(Edit::Delete));
    assert_eq!(u.text, "a");
    assert!(!u.edit(Edit::Delete));
    assert!(u.edit(Edit::Backspace));
    assert!(!u.edit(Edit::Backspace));
    assert_eq!(u.text, "");
}

#[test]
fn arrows_home_and_end_move_without_changing_the_text() {
    let mut u = typed("ab");
    assert!(!u.edit(mv(Motion::Left)));
    assert_eq!((u.caret, u.anchor), (1, 1));
    u.edit(mv(Motion::Home));
    u.edit(mv(Motion::Left));
    assert_eq!(u.caret, 0);
    u.edit(mv(Motion::End));
    u.edit(mv(Motion::Right));
    assert_eq!((u.caret, u.text.as_str()), (2, "ab"));
}

#[test]
fn select_all_then_typing_replaces_and_arrows_collapse() {
    let mut u = typed("old");
    u.edit(Edit::SelectAll);
    assert_eq!(u.selection(), Some((0, 3)));
    assert!(u.edit(Edit::Char('n')));
    assert_eq!((u.text.as_str(), u.selection()), ("n", None));
    let mut u = typed("old");
    u.edit(Edit::SelectAll);
    assert!(u.edit(Edit::Backspace));
    assert_eq!(u.text, "");
    let mut u = typed("old");
    u.edit(Edit::SelectAll);
    u.edit(mv(Motion::Left));
    assert_eq!((u.caret, u.text.as_str(), u.selection()), (0, "old", None));
    u.edit(Edit::SelectAll);
    u.edit(mv(Motion::Right));
    assert_eq!((u.caret, u.selection()), (3, None));
}

#[test]
fn shift_motions_extend_from_the_anchor_in_both_directions() {
    let mut u = typed("abcd");
    u.edit(ext(Motion::Left));
    u.edit(ext(Motion::Left));
    assert_eq!((u.selected_text(), u.anchor, u.caret), (Some("cd"), 4, 2));
    u.edit(ext(Motion::Right));
    assert_eq!(u.selected_text(), Some("d"));
    u.edit(ext(Motion::Home));
    assert_eq!(u.selected_text(), Some("abcd"), "the anchor stays at the end");
    u.edit(mv(Motion::Right)); // collapses to the selection's right edge
    assert_eq!((u.caret, u.selection()), (4, None));
}

#[test]
fn typing_over_part_of_the_text_replaces_just_that_part() {
    let mut u = typed("abcd");
    u.edit(Edit::Place { at: 1, extend: false });
    u.edit(Edit::Place { at: 3, extend: true });
    assert_eq!(u.selected_text(), Some("bc"));
    assert!(u.edit(Edit::Char('X')));
    assert_eq!((u.text.as_str(), u.caret, u.selection()), ("aXd", 2, None));
    u.edit(Edit::Place { at: 0, extend: false });
    u.edit(Edit::Place { at: 2, extend: true });
    assert!(u.edit(Edit::Delete));
    assert_eq!(u.text, "d");
}

#[test]
fn placing_lands_on_a_character_boundary_and_clamps_to_the_text() {
    let mut u = typed("a\u{e9}b"); // e-acute is two bytes: 1..3
    u.edit(Edit::Place { at: 2, extend: false });
    assert_eq!(u.caret, 1);
    u.edit(Edit::Place { at: 99, extend: false });
    assert_eq!((u.caret, u.anchor), (4, 4));
}

#[test]
fn double_click_selects_the_word_run_or_gap_under_the_pointer() {
    let mut u = typed("foo_1  bar.baz");
    u.edit(Edit::Word(2));
    assert_eq!(u.selected_text(), Some("foo_1"));
    u.edit(Edit::Word(5));
    assert_eq!(u.selected_text(), Some("  "), "a gap selects the gap");
    u.edit(Edit::Word(10));
    assert_eq!(u.selected_text(), Some("."));
    u.edit(Edit::Word(14));
    assert_eq!(u.selected_text(), Some("baz"), "at the end the last word is picked");
    let mut e = FilterUi::default();
    e.edit(Edit::Word(0));
    assert_eq!(e.selection(), None);
}

#[test]
fn paste_drops_line_breaks_and_controls() {
    let mut u = typed("ab");
    u.edit(mv(Motion::Left));
    assert!(u.edit(Edit::Paste("x\r\ny\tz".into())));
    assert_eq!((u.text.as_str(), u.caret), ("axyzb", 4));
}

#[test]
fn control_characters_are_ignored_and_keep_a_selection() {
    let mut u = typed("ab");
    u.edit(Edit::SelectAll);
    assert!(!u.edit(Edit::Char('\u{8}')));
    assert_eq!(u.selection(), Some((0, 2)));
    assert_eq!(u.text, "ab");
}

#[test]
fn the_text_is_capped() {
    let mut u = FilterUi::default();
    assert!(u.edit(Edit::Paste("a".repeat(MAX_CHARS + 50))));
    assert_eq!(u.text.chars().count(), MAX_CHARS);
    assert!(!u.edit(Edit::Char('b')));
    u.edit(Edit::SelectAll);
    assert!(u.edit(Edit::Paste("b".repeat(MAX_CHARS + 5))), "the cap counts what is left after the selection goes");
    assert_eq!(u.text.chars().count(), MAX_CHARS);
}

#[test]
fn clear_reports_whether_there_was_text() {
    let mut u = typed("x");
    assert!(u.clear());
    assert!(!u.clear());
    assert_eq!((u.text.as_str(), u.caret, u.anchor), ("", 0, 0));
}

#[test]
fn byte_at_picks_the_nearest_boundary() {
    let w = |s: &str| s.chars().count() as f32 * 10.0;
    assert_eq!(byte_at("a\u{e9}b", -5.0, &w), 0);
    assert_eq!(byte_at("a\u{e9}b", 4.0, &w), 0);
    assert_eq!(byte_at("a\u{e9}b", 6.0, &w), 1);
    assert_eq!(byte_at("a\u{e9}b", 16.0, &w), 3, "past the e-acute lands after its two bytes");
    assert_eq!(byte_at("a\u{e9}b", 500.0, &w), 4);
    assert_eq!(byte_at("", 20.0, &w), 0);
}

#[test]
fn utf16_units_combine_into_characters() {
    let mut p = None;
    assert_eq!(push_utf16(&mut p, 'a' as u16), Some('a'));
    let pair: Vec<u16> = "\u{1F600}".encode_utf16().collect();
    assert_eq!(push_utf16(&mut p, pair[0]), None);
    assert_eq!(push_utf16(&mut p, pair[1]), Some('\u{1F600}'));
    assert_eq!(push_utf16(&mut p, pair[1]), None); // a lone low half is dropped
    push_utf16(&mut p, pair[0]);
    assert_eq!(push_utf16(&mut p, 'b' as u16), Some('b')); // a lone high half is dropped
}
