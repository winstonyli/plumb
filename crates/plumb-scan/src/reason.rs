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
    pub fn other(text: impl Into<String>) -> Self {
        Reason::Other(text.into())
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
        assert!(Reason::AccessDenied.is_access_denied() && !Reason::other("access denied").is_access_denied());
    }
}
