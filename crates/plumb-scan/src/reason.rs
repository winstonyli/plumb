//! Why something could not be done to a file or a process, in words. One type for both so the dialogs word a failure
//! one way and act on "access denied" without matching text.
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The system refused the caller: a protected or elevated process, or a file it may not touch.
    AccessDenied,
    /// Anything else, as the words to show.
    Other(String),
}

impl Reason {
    /// `text` is cleaned to run into a sentence: system messages end in a full stop, may hold line breaks and start with a
    /// capital (kept for an acronym like "DLL"), and the dialogs add their own punctuation and capital.
    pub fn other(text: impl AsRef<str>) -> Self {
        let words: Vec<&str> = text.as_ref().split_whitespace().collect();
        let text = words.join(" ");
        let text = text.trim_end_matches('.');
        let mut c = text.chars();
        let lowered = match (c.next(), c.next()) {
            (Some(a), Some(b)) if a.is_uppercase() && !b.is_uppercase() => format!("{}{}{}", a.to_lowercase(), b, c.as_str()),
            _ => text.to_string(),
        };
        Reason::Other(lowered)
    }

    pub fn is_access_denied(&self) -> bool {
        *self == Reason::AccessDenied
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reason::AccessDenied => f.write_str("access denied"),
            Reason::Other(text) => f.write_str(text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_reads_as_its_words_and_only_access_denied_says_so() {
        assert_eq!(Reason::AccessDenied.to_string(), "access denied");
        assert_eq!(Reason::other("in use").to_string(), "in use");
        assert_eq!(Reason::other("The file is in use.\r\n").to_string(), "the file is in use", "no full stop or line break to double up, and no capital mid-line");
        assert_eq!(Reason::other("DLL not found").to_string(), "DLL not found", "an acronym keeps its capitals");
        assert_eq!(Reason::other("").to_string(), "");
        assert!(Reason::AccessDenied.is_access_denied() && !Reason::other("access denied").is_access_denied());
    }
}
