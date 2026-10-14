//! The filter text box as a pure reducer: no Win32 types, so every edit tests anywhere.

/// Longest query accepted, in characters: a guard against pasting a book into the box.
pub const MAX_CHARS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Home,
    End,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    Char(char),
    Backspace,
    Delete,
    /// Moves the caret; `extend` keeps the anchor, so the text between them becomes selected.
    Move { to: Motion, extend: bool },
    SelectAll,
    /// Pasted text; line breaks and other control characters are dropped.
    Paste(String),
    /// The mouse pressed (or, with `extend`, dragged or shift-clicked) at this byte offset.
    Place { at: usize, extend: bool },
    /// A double-click at this byte offset: selects the word there.
    Word(usize),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilterUi {
    pub text: String,
    /// Caret position as a byte offset on a char boundary: the moving end of the selection.
    pub caret: usize,
    /// The fixed end of the selection; the same as `caret` when nothing is selected.
    pub anchor: usize,
    pub regex: bool,
    pub focus: bool,
}

impl FilterUi {
    /// The selected byte range, if any.
    pub fn selection(&self) -> Option<(usize, usize)> {
        (self.anchor != self.caret).then(|| (self.anchor.min(self.caret), self.anchor.max(self.caret)))
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|(a, b)| &self.text[a..b])
    }

    /// Applies `e`; true if the text changed.
    pub fn edit(&mut self, e: Edit) -> bool {
        let sel = self.selection();
        match e {
            Edit::Char(c) => self.insert(c.encode_utf8(&mut [0; 4])),
            Edit::Paste(s) => self.insert(&s),
            Edit::Backspace | Edit::Delete if sel.is_some() => self.delete_selection(),
            Edit::Backspace => match self.prev(self.caret) {
                Some(p) => {
                    self.text.drain(p..self.caret);
                    self.caret = p;
                    self.anchor = p;
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
            Edit::Move { to, extend } => {
                let at = match (to, sel, extend) {
                    (Motion::Left, Some((a, _)), false) => a,
                    (Motion::Right, Some((_, b)), false) => b,
                    (Motion::Left, ..) => self.prev(self.caret).unwrap_or(0),
                    (Motion::Right, ..) => self.next(self.caret).unwrap_or(self.text.len()),
                    (Motion::Home, ..) => 0,
                    (Motion::End, ..) => self.text.len(),
                };
                self.place(at, extend);
                false
            }
            Edit::SelectAll => {
                self.anchor = 0;
                self.caret = self.text.len();
                false
            }
            Edit::Place { at, extend } => {
                self.place(at, extend);
                false
            }
            Edit::Word(at) => {
                let at = self.boundary(at);
                let class = |c: char| (c.is_alphanumeric() || c == '_', c.is_whitespace());
                let t = &self.text;
                let here = t[at..].chars().next().or_else(|| t[..at].chars().next_back());
                if let Some(h) = here {
                    // On the left half of a boundary the character after it decides; at the end, the one before.
                    let start_at = if t[at..].is_empty() { at - h.len_utf8() } else { at };
                    let k = class(h);
                    let a = t[..start_at].char_indices().rev().take_while(|&(_, c)| class(c) == k).last().map_or(start_at, |(i, _)| i);
                    let b = t[start_at..].char_indices().find(|&(_, c)| class(c) != k).map_or(t.len(), |(i, _)| start_at + i);
                    (self.anchor, self.caret) = (a, b);
                }
                false
            }
        }
    }

    /// Empties the box; true if there was text.
    pub fn clear(&mut self) -> bool {
        let had = !self.text.is_empty();
        self.text.clear();
        self.caret = 0;
        self.anchor = 0;
        had
    }

    /// `at` rounded down to a character boundary within the text.
    fn boundary(&self, at: usize) -> usize {
        let mut at = at.min(self.text.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    fn place(&mut self, at: usize, extend: bool) {
        self.caret = self.boundary(at);
        if !extend {
            self.anchor = self.caret;
        }
    }

    fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else { return false };
        self.text.drain(a..b);
        self.caret = a;
        self.anchor = a;
        true
    }

    fn insert(&mut self, s: &str) -> bool {
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        if clean.is_empty() {
            return false; // an ignored key keeps the selection
        }
        let replaced = self.delete_selection();
        let room = MAX_CHARS.saturating_sub(self.text.chars().count());
        let clean: String = clean.chars().take(room).collect();
        self.text.insert_str(self.caret, &clean);
        self.caret += clean.len();
        self.anchor = self.caret;
        replaced || !clean.is_empty()
    }

    fn prev(&self, at: usize) -> Option<usize> {
        self.text[..at].char_indices().next_back().map(|(i, _)| i)
    }

    fn next(&self, at: usize) -> Option<usize> {
        self.text[at..].chars().next().map(|c| at + c.len_utf8())
    }
}

/// The byte offset of the character boundary nearest `x` pixels from the start of `text`, where
/// `width` measures a string.
pub fn byte_at(text: &str, x: f32, width: &dyn Fn(&str) -> f32) -> usize {
    let ends = text.char_indices().map(|(i, _)| i).skip(1).chain([text.len()]);
    let mut best = (x.abs(), 0); // the start, 0 px in
    for i in ends.filter(|&i| i > 0) {
        let d = (width(&text[..i]) - x).abs();
        if d < best.0 {
            best = (d, i);
        }
    }
    best.1
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
mod tests;
