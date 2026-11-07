//! What a row can say about itself besides its name and numbers (denied, suspended, ...): one list per row, read by the
//! row painter (pills), the row tooltip, the map tooltip and the status line, so an attribute is added in one place.
use crate::app::ProcInfo;
use plumb_core::{NodeFlags, NodeId, Tree};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attr {
    /// A folder whose contents could not be read.
    Denied,
    /// A process plumb cannot open (another user's, or protected).
    Protected,
    Suspended,
    /// A process running as administrator.
    Elevated,
    /// A junction or symbolic link: not followed.
    Link,
    /// A further hard link to a file already counted.
    Hardlink,
    /// A hidden or system file or folder.
    System,
    /// A cloud placeholder: its data is not on this disk until it is opened.
    Cloud,
}

impl Attr {
    /// Every attribute, in the order its pill is drawn, left to right.
    pub const ALL: [Attr; 8] = [Attr::Denied, Attr::Protected, Attr::Suspended, Attr::Elevated, Attr::Link, Attr::Hardlink, Attr::System, Attr::Cloud];

    /// The pill's text.
    pub fn word(self) -> &'static str {
        match self {
            Attr::Denied => "denied",
            Attr::Protected => "protected",
            Attr::Suspended => "suspended",
            Attr::Elevated => "elevated",
            Attr::Link => "link",
            Attr::Hardlink => "hardlink",
            Attr::System => "system",
            Attr::Cloud => "cloud",
        }
    }

    /// One sentence for the tooltip.
    pub fn meaning(self) -> &'static str {
        match self {
            Attr::Denied => "its contents could not be read.",
            Attr::Protected => "plumb cannot open this process (another user's, or protected); run plumb as administrator to reach more.",
            Attr::Suspended => "every thread is suspended; Resume process continues it.",
            Attr::Elevated => "runs as administrator; plumb needs administrator rights to end, suspend or resume it.",
            Attr::Link => "a junction or symbolic link; plumb does not follow it.",
            Attr::Hardlink => "a further hard link to a file already counted; its size is not counted again.",
            Attr::System => "a hidden or system file or folder.",
            Attr::Cloud => "a cloud placeholder; its data is not on this disk until it is opened, so it takes little or no space here.",
        }
    }
}

/// A set of attributes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attrs(u8);

impl Attrs {
    fn bit(a: Attr) -> u8 {
        1 << Attr::ALL.iter().position(|&x| x == a).unwrap_or(0)
    }

    fn with(mut self, a: Attr, on: bool) -> Self {
        if on {
            self.0 |= Self::bit(a);
        }
        self
    }

    #[cfg(test)]
    pub fn of_list(list: &[Attr]) -> Self {
        list.iter().fold(Self::default(), |a, &x| a.with(x, true))
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn contains(self, a: Attr) -> bool {
        self.0 & Self::bit(a) != 0
    }

    /// The attributes in the order they are drawn.
    pub fn iter(self) -> impl DoubleEndedIterator<Item = Attr> {
        Attr::ALL.into_iter().filter(move |&a| self.contains(a))
    }

    /// "suspended, link": the words, for the status line.
    pub fn words(self) -> String {
        self.iter().map(Attr::word).collect::<Vec<_>>().join(", ")
    }
}

/// The attributes of row `id`. `procs` is the Processes view's per-process facts, `None` in the other views.
pub fn of(tree: &Tree, id: NodeId, procs: Option<&HashMap<u32, ProcInfo>>) -> Attrs {
    let flags = tree.flags[id];
    let mut a = Attrs::default().with(Attr::Denied, flags.contains(NodeFlags::DENIED)).with(Attr::Link, flags.contains(NodeFlags::REPARSE)).with(Attr::Hardlink, flags.contains(NodeFlags::HARDLINK)).with(Attr::System, flags.contains(NodeFlags::SYSTEM)).with(Attr::Cloud, flags.contains(NodeFlags::CLOUD));
    if let Some(procs) = procs {
        let pid = tree.aux.get(id.idx()).map_or(0, |x| x[2]) as u32;
        let info = procs.get(&pid);
        a = a.with(Attr::Suspended, info.is_some_and(|i| i.suspended)).with(Attr::Protected, pid > 0 && info.is_some_and(|i| i.path.is_none())).with(Attr::Elevated, info.is_some_and(|i| i.elevated));
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_lists_its_attributes_in_pill_order() {
        let a = Attrs::default().with(Attr::Hardlink, true).with(Attr::Suspended, true).with(Attr::Denied, false);
        assert_eq!(a.iter().collect::<Vec<_>>(), vec![Attr::Suspended, Attr::Hardlink]);
        assert_eq!(a.words(), "suspended, hardlink");
        assert!(Attrs::default().is_empty() && !a.is_empty());
    }

    #[test]
    fn every_attribute_has_its_own_bit_in_the_set() {
        assert!(Attr::ALL.len() <= 8, "Attrs is a u8");
        let all = Attr::ALL.iter().fold(Attrs::default(), |a, &x| a.with(x, true));
        assert_eq!(all.iter().count(), Attr::ALL.len());
    }
}
