//! Bridges the app crate's std-mpsc pty reader into an iced futures [`Stream`].
//!
//! One thread per pane coalesces whatever the reader has queued into one message, so
//! a burst of output costs one iced update; the pane tag lives in the slot.

use std::pin::Pin;
use std::sync::{mpsc, Arc, Mutex};

use ghostwave::pty::PtyMsg;
use iced::futures::channel::mpsc as fmpsc;
use iced::futures::{Stream, StreamExt};

use crate::Message;

/// Cap, in bytes, on how much coalesced output one iced message carries.
const COALESCE_CAP: usize = 256 * 1024;

/// The pane a message came from, by process-wide [`PaneId`](crate::pane::PaneId)
/// rather than by position in a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PaneTag(pub crate::pane::PaneId);

#[derive(Debug)]
enum BridgeMsg {
    Data(Vec<u8>),
    Exited(String),
}

struct Slot {
    tag: PaneTag,
    rx: Mutex<Option<fmpsc::UnboundedReceiver<BridgeMsg>>>,
}

/// A pane's iced-side receiver; hashes by pointer so the subscription identity is
/// stable across view rebuilds.
#[derive(Clone)]
pub struct PtyRxSlot(Arc<Slot>);

impl std::hash::Hash for PtyRxSlot {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.0) as usize).hash(state);
    }
}

/// Starts the thread that forwards `rx` into an iced channel, tagging its output with
/// `id`.
pub fn spawn(id: crate::pane::PaneId, rx: mpsc::Receiver<PtyMsg>) -> PtyRxSlot {
    let (tx, out_rx) = fmpsc::unbounded::<BridgeMsg>();

    let spawned = std::thread::Builder::new()
        .name(format!("ghost-iced-pty-{}", id.0))
        .spawn(move || {
            let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
            loop {
                match rx.recv() {
                    Ok(PtyMsg::Data(data)) => {
                        buf.extend_from_slice(&data);
                        // Coalesce what is already queued behind it.
                        let mut eof: Option<String> = None;
                        while buf.len() < COALESCE_CAP {
                            match rx.try_recv() {
                                Ok(PtyMsg::Data(more)) => buf.extend_from_slice(&more),
                                Ok(PtyMsg::Eof) => {
                                    eof = Some("process exited".into());
                                    break;
                                }
                                Ok(PtyMsg::Error(e)) => {
                                    eof = Some(format!("pty error: {e}"));
                                    break;
                                }
                                Err(_) => break,
                            }
                        }
                        if !buf.is_empty()
                            && tx.unbounded_send(BridgeMsg::Data(std::mem::take(&mut buf))).is_err()
                        {
                            return;
                        }
                        if let Some(reason) = eof {
                            let _ = tx.unbounded_send(BridgeMsg::Exited(reason));
                            return;
                        }
                    }
                    Ok(PtyMsg::Eof) => {
                        let _ = tx.unbounded_send(BridgeMsg::Exited("process exited".into()));
                        return;
                    }
                    Ok(PtyMsg::Error(e)) => {
                        let _ = tx.unbounded_send(BridgeMsg::Exited(format!("pty error: {e}")));
                        return;
                    }
                    Err(_) => {
                        let _ = tx.unbounded_send(BridgeMsg::Exited("pty closed".into()));
                        return;
                    }
                }
            }
        });

    if let Err(e) = spawned {
        eprintln!("[ghost-iced] pty bridge thread failed to start: {e}");
    }

    PtyRxSlot(Arc::new(Slot {
        tag: PaneTag(id),
        rx: Mutex::new(Some(out_rx)),
    }))
}

/// The coalesced pty stream for a pane; a second call (a subscription restart after
/// the receiver was taken) yields a pending stream instead of panicking.
pub fn stream(slot: &PtyRxSlot) -> Pin<Box<dyn Stream<Item = Message> + Send>> {
    let id = slot.0.tag.0;
    match slot.0.rx.lock().unwrap().take() {
        Some(rx) => Box::pin(rx.map(move |msg| match msg {
            BridgeMsg::Data(data) => Message::Output { id, data },
            BridgeMsg::Exited(reason) => Message::Exited { id, reason },
        })),
        None => Box::pin(iced::futures::stream::pending::<Message>()),
    }
}
