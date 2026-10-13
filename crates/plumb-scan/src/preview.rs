//! What a running scan tells the window so it can show a partial tree. Best effort: sends never
//! block the scanner. Once the queue is full the sender is `lagged` for good and drops everything,
//! so the receiver's prefix stays consistent (the walker's node ids depend on it).
use crate::journal::state::FileState;
use plumb_core::{NodeFlags, NodeId};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

#[derive(Debug)]
pub enum PreviewMsg {
    /// MFT scan: the root's name and how many record slots there are (a capacity hint).
    Begin { root: String, nodes: usize },
    /// Walker: a node was pushed. Ids follow push order, so the receiver's ids equal the builder's.
    /// The first message has parent `NodeId::NONE` and is the root.
    Add { parent: NodeId, name: Vec<u16>, size: u64, flags: NodeFlags },
    /// Walker: the directory could not be listed.
    Denied(NodeId),
    /// MFT scan: a base record's file, with the name that carries its size.
    Upsert(FileState),
    /// MFT scan: a size found in an extension record of `record`.
    Size { record: u64, size: u64 },
    /// A scanner gave up and another starts: forget everything received so far.
    Reset,
}

#[derive(Clone)]
pub struct PreviewTx {
    tx: SyncSender<PreviewMsg>,
    lagged: Arc<AtomicBool>,
}

pub struct PreviewRx {
    rx: Receiver<PreviewMsg>,
    lagged: Arc<AtomicBool>,
}

/// A channel holding at most `cap` messages.
pub fn channel(cap: usize) -> (PreviewTx, PreviewRx) {
    let (tx, rx) = sync_channel(cap);
    let lagged = Arc::new(AtomicBool::new(false));
    (PreviewTx { tx, lagged: lagged.clone() }, PreviewRx { rx, lagged })
}

impl PreviewTx {
    pub fn lagged(&self) -> bool {
        self.lagged.load(Ordering::Relaxed)
    }

    /// Queues `msg`, or marks the sender lagged (dropping `msg` and every later message) if the
    /// queue is full or the receiver is gone.
    pub fn send(&self, msg: PreviewMsg) {
        if self.lagged() {
            return;
        }
        match self.tx.try_send(msg) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.lagged.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Tells the receiver to start over, and lets later messages through again. If the queue is
    /// too full to take the `Reset`, the sender stays lagged.
    pub fn reset(&self) {
        self.lagged.store(false, Ordering::Relaxed);
        self.send(PreviewMsg::Reset);
    }
}

impl PreviewRx {
    pub fn lagged(&self) -> bool {
        self.lagged.load(Ordering::Relaxed)
    }

    /// `None` when nothing is queued (or the sender is gone).
    pub fn try_recv(&self) -> Option<PreviewMsg> {
        self.rx.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_queue_marks_the_sender_lagged_and_drops_everything_after() {
        let (tx, rx) = channel(2);
        for i in 0..5u64 {
            tx.send(PreviewMsg::Size { record: i, size: 0 });
        }
        assert!(tx.lagged() && rx.lagged());
        // Draining makes room, but nothing sent after the first drop may get through.
        let got: Vec<u64> = std::iter::from_fn(|| rx.try_recv())
            .map(|m| match m {
                PreviewMsg::Size { record, .. } => record,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(got, [0, 1]);
        tx.send(PreviewMsg::Size { record: 9, size: 0 });
        assert!(rx.try_recv().is_none());
    }

    #[test]
    fn reset_lets_messages_through_again() {
        let (tx, rx) = channel(2);
        for i in 0..5u64 {
            tx.send(PreviewMsg::Size { record: i, size: 0 });
        }
        while rx.try_recv().is_some() {}
        tx.reset();
        tx.send(PreviewMsg::Size { record: 7, size: 0 });
        assert!(matches!(rx.try_recv(), Some(PreviewMsg::Reset)));
        assert!(matches!(rx.try_recv(), Some(PreviewMsg::Size { record: 7, .. })));
        assert!(!tx.lagged());
    }

    #[test]
    fn a_dropped_receiver_lags_the_sender_without_panicking() {
        let (tx, rx) = channel(2);
        drop(rx);
        tx.send(PreviewMsg::Reset);
        assert!(tx.lagged());
    }
}
