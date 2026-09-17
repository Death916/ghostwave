//! Bridge between the app crate's std-mpsc pty reader and iced's executor.
//!
//! `ghostwave::pty::Pty::spawn_reader` hands back a `std::sync::mpsc::Receiver`
//! and blocks on the master fd; iced wants a futures `Stream`. A dedicated
//! thread moves the bytes across, greedily coalescing whatever the reader has
//! already queued into one message so a burst of output costs one iced update
//! (and therefore one repaint) instead of one per 64 KiB chunk.
//!
//! With several panes alive there is one bridge per pane, and messages carry the
//! pane they came from: the tag lives in the slot, so `stream` can build the
//! right `Message` without the bridge thread knowing anything about the app.

use std::pin::Pin;
use std::sync::{mpsc, Arc, Mutex};

use ghostwave::pty::PtyMsg;
use iced::futures::channel::mpsc as fmpsc;
use iced::futures::{Stream, StreamExt};

use crate::Message;

/// Cap on how much coalesced output one iced message may carry.
const COALESCE_CAP: usize = 256 * 1024;

/// Which pane a message belongs to. Pane ids are indices into the tab's pane
/// vec and are STABLE for the life of the pane (closed panes leave a tombstone
/// rather than renumbering), because a bridge's tag cannot be changed after the
/// subscription has taken its receiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PaneTag {
    pub tab: usize,
    pub pane: usize,
}

/// What came across from the reader thread.
#[derive(Debug)]
enum BridgeMsg {
    Data(Vec<u8>),
    Exited(String),
}

struct Slot {
    tag: PaneTag,
    rx: Mutex<Option<fmpsc::UnboundedReceiver<BridgeMsg>>>,
}

/// A pane's iced-side receiver. Wrapped in an `Arc` and hashed by pointer so
/// iced treats the subscription identity as stable across view rebuilds.
#[derive(Clone)]
pub struct PtyRxSlot(Arc<Slot>);

impl std::hash::Hash for PtyRxSlot {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.0) as usize).hash(state);
    }
}

impl PtyRxSlot {
    /// Which pane this slot's stream belongs to.
    #[allow(dead_code)]
    pub fn tag(&self) -> PaneTag {
        self.0.tag
    }
}

/// Start the bridge thread that forwards the pty reader into an iced channel.
pub fn spawn(tag: PaneTag, rx: mpsc::Receiver<PtyMsg>) -> PtyRxSlot {
    let (tx, out_rx) = fmpsc::unbounded::<BridgeMsg>();

    let spawned = std::thread::Builder::new()
        .name(format!("ghost-iced-pty-{}-{}", tag.tab, tag.pane))
        .spawn(move || {
            let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
            loop {
                match rx.recv() {
                    Ok(PtyMsg::Data(data)) => {
                        buf.extend_from_slice(&data);
                        // Coalesce everything else already queued behind it.
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
        tag,
        rx: Mutex::new(Some(out_rx)),
    }))
}

/// Built by iced when the subscription starts: the coalesced pty stream, tagged
/// with the pane it belongs to. A later call (subscription restart after the
/// receiver was taken) yields a pending stream instead of panicking.
pub fn stream(slot: &PtyRxSlot) -> Pin<Box<dyn Stream<Item = Message> + Send>> {
    let tag = slot.0.tag;
    match slot.0.rx.lock().unwrap().take() {
        Some(rx) => Box::pin(rx.map(move |msg| match msg {
            BridgeMsg::Data(data) => Message::Output {
                tab: tag.tab,
                pane: tag.pane,
                data,
            },
            BridgeMsg::Exited(reason) => Message::Exited {
                tab: tag.tab,
                pane: tag.pane,
                reason,
            },
        })),
        None => Box::pin(iced::futures::stream::pending::<Message>()),
    }
}
