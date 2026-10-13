//! The filter text box as a pure reducer: no Win32 types, so every edit tests anywhere.

/// Longest query accepted, in characters: a guard against pasting a book into the box.
pub const MAX_CHARS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    Char(char),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    SelectAll,
    /// Pasted text; line breaks and other control characters are dropped.
    Paste(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilterUi {
    pub text: String,
    /// Caret position as a byte offset on a char boundary.
    pub caret: usize,
    pub regex: bool,
    pub focus: bool,
    /// The whole text is selected: the next character or paste replaces it.
    pub all: bool,
}

impl FilterUi {
    /// Applies `e`; true if the text changed.
    pub fn edit(&mut self, e: Edit) -> bool {
        let selected = std::mem::take(&mut self.all);
        match e {
            Edit::Char(c) => self.insert(c.encode_utf8(&mut [0; 4]), selected),
            Edit::Paste(s) => self.insert(&s, selected),
            Edit::Backspace | Edit::Delete if selected => self.clear(),
            Edit::Backspace => match self.prev(self.caret) {
                Some(p) => {
                    self.text.drain(p..self.caret);
                    self.caret = p;
                    true
                }
                None => false,
            },
            Edit::Delete => match self.next(self.caret) {
                Some(n) => {
                    self.text.drain(self.caret..n);
                    true
                }
                None => false,
            },
            Edit::Left => {
                self.caret = if selected { 0 } else { self.prev(self.caret).unwrap_or(0) };
                false
            }
            Edit::Right => {
                self.caret = if selected { self.text.len() } else { self.next(self.caret).unwrap_or(self.text.len()) };
                false
            }
            Edit::Home => {
                self.caret = 0;
                false
            }
            Edit::End => {
                self.caret = self.text.len();
                false
            }
            Edit::SelectAll => {
                self.all = !self.text.is_empty();
                false
            }
        }
    }

    /// Empties the box; true if there was text.
    pub fn clear(&mut self) -> bool {
        let had = !self.text.is_empty();
        self.text.clear();
        self.caret = 0;
        self.all = false;
        had
    }

    fn insert(&mut self, s: &str, replace: bool) -> bool {
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        if clean.is_empty() {
            self.all = replace; // an ignored key keeps the selection
            return false;
        }
        if replace {
            self.text.clear();
            self.caret = 0;
        }
        let room = MAX_CHARS.saturating_sub(self.text.chars().count());
        let clean: String = clean.chars().take(room).collect();
        self.text.insert_str(self.caret, &clean);
        self.caret += clean.len();
        replace || !clean.is_empty()
    }

    fn prev(&self, at: usize) -> Option<usize> {
        self.text[..at].char_indices().next_back().map(|(i, _)| i)
    }

    fn next(&self, at: usize) -> Option<usize> {
        self.text[at..].chars().next().map(|c| at + c.len_utf8())
    }
}

/// Folds the UTF-16 units of `WM_CHAR` into characters: a high surrogate is held in `pending` until
/// its low half arrives. A lone surrogate is dropped.
pub fn push_utf16(pending: &mut Option<u16>, unit: u16) -> Option<char> {
    match unit {
        0xD800..=0xDBFF => {
            *pending = Some(unit);
            None
        }
        0xDC00..=0xDFFF => {
            let high = pending.take()?;
            char::decode_utf16([high, unit]).next()?.ok()
        }
        _ => {
            *pending = None;
            char::from_u32(unit as u32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> FilterUi {
        let mut u = FilterUi::default();
        for c in s.chars() {
            u.edit(Edit::Char(c));
        }
        u
    }

    #[test]
    fn typing_inserts_at_the_caret_and_reports_a_change() {
        let mut u = typed("ac");
        u.edit(Edit::Left);
        assert!(u.edit(Edit::Char('b')));
        assert_eq!((u.text.as_str(), u.caret), ("abc", 2));
    }

    #[test]
    fn backspace_and_delete_remove_whole_characters() {
        let mut u = typed("a\u{e9}\u{1F600}");
        assert!(u.edit(Edit::Backspace));
        assert_eq!(u.text, "a\u{e9}");
        u.edit(Edit::Left); // before the e-acute
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
        assert!(!u.edit(Edit::Left));
        assert_eq!(u.caret, 1);
        u.edit(Edit::Home);
        u.edit(Edit::Left);
        assert_eq!(u.caret, 0);
        u.edit(Edit::End);
        u.edit(Edit::Right);
        assert_eq!((u.caret, u.text.as_str()), (2, "ab"));
    }

    #[test]
    fn select_all_then_typing_replaces_and_arrows_collapse() {
        let mut u = typed("old");
        u.edit(Edit::SelectAll);
        assert!(u.all);
        assert!(u.edit(Edit::Char('n')));
        assert_eq!((u.text.as_str(), u.all), ("n", false));
        let mut u = typed("old");
        u.edit(Edit::SelectAll);
        assert!(u.edit(Edit::Backspace));
        assert_eq!(u.text, "");
        let mut u = typed("old");
        u.edit(Edit::SelectAll);
        u.edit(Edit::Left);
        assert_eq!((u.caret, u.text.as_str(), u.all), (0, "old", false));
        u.edit(Edit::SelectAll);
        u.edit(Edit::Right);
        assert_eq!(u.caret, 3);
    }

    #[test]
    fn paste_drops_line_breaks_and_controls() {
        let mut u = typed("ab");
        u.edit(Edit::Left);
        assert!(u.edit(Edit::Paste("x\r\ny\tz".into())));
        assert_eq!((u.text.as_str(), u.caret), ("axyzb", 4));
    }

    #[test]
    fn control_characters_are_ignored_and_keep_a_selection() {
        let mut u = typed("ab");
        u.edit(Edit::SelectAll);
        assert!(!u.edit(Edit::Char('\u{8}')));
        assert!(u.all);
        assert_eq!(u.text, "ab");
    }

    #[test]
    fn the_text_is_capped() {
        let mut u = FilterUi::default();
        assert!(u.edit(Edit::Paste("a".repeat(MAX_CHARS + 50))));
        assert_eq!(u.text.chars().count(), MAX_CHARS);
        assert!(!u.edit(Edit::Char('b')));
    }

    #[test]
    fn clear_reports_whether_there_was_text() {
        let mut u = typed("x");
        assert!(u.clear());
        assert!(!u.clear());
        assert_eq!((u.text.as_str(), u.caret), ("", 0));
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
}
