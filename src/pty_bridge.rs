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

/// Cap, in bytes, on **every** coalesced output message this bridge sends. The
/// coalesce loop stops extending past it, and a reader chunk larger than the cap is
/// split before it is sent, so one iced update's `vt_write` never exceeds it — that
/// is the longest a key can wait behind one update.
const COALESCE_CAP: usize = 32 * 1024;

/// Messages in flight between the bridge thread and the UI. With every message at
/// most [`COALESCE_CAP`], this is a hard bound of 2 MiB; a full queue parks the
/// bridge thread, which is the backpressure chain back to the pty.
const COALESCE_QUEUE: usize = 64;

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
    rx: Mutex<Option<fmpsc::Receiver<BridgeMsg>>>,
    /// Keeps an idle slot's channel open, so a pane that has no child yet
    /// streams nothing instead of ending. Held for its drop, never read.
    #[allow(dead_code)]
    keepalive: Option<fmpsc::Sender<BridgeMsg>>,
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

/// Hands `msg` to the UI, waiting (1 ms at a time) while the queue is full so a
/// flood applies backpressure to the pty instead of growing a queue nobody asked
/// for. Returns false once the UI side is gone.
fn send_blocking(tx: &mut fmpsc::Sender<BridgeMsg>, mut msg: BridgeMsg) -> bool {
    loop {
        match tx.try_send(msg) {
            Ok(()) => return true,
            Err(e) if e.is_full() => {
                msg = e.into_inner();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(_) => return false,
        }
    }
}

/// Sends `buf` as one or more `COALESCE_CAP`-sized data messages, then empties it.
/// A single reader chunk can exceed the cap (`spawn_reader` reads up to 64 KiB,
/// `src/pty.rs:133`), so the split is what makes "at most `COALESCE_CAP` per
/// `Message::Output`" true instead of merely typical. Returns false once the UI
/// side is gone.
fn send_data(tx: &mut fmpsc::Sender<BridgeMsg>, buf: &mut Vec<u8>) -> bool {
    let mut start = 0;
    while start < buf.len() {
        let end = (start + COALESCE_CAP).min(buf.len());
        if !send_blocking(tx, BridgeMsg::Data(buf[start..end].to_vec())) {
            return false;
        }
        start = end;
    }
    buf.clear();
    true
}

/// Starts the thread that forwards `rx` into an iced channel, tagging its output with
/// `id`.
pub fn spawn(id: crate::pane::PaneId, rx: mpsc::Receiver<PtyMsg>) -> PtyRxSlot {
    let (mut tx, out_rx) = fmpsc::channel::<BridgeMsg>(COALESCE_QUEUE);

    let spawned = std::thread::Builder::new()
        .name(format!("ghost-iced-pty-{}", id.0))
        .spawn(move || {
            let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
            loop {
                match rx.recv() {
                    Ok(PtyMsg::Data(data)) => {
                        buf.extend_from_slice(&data);
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
                        if !buf.is_empty() && !send_data(&mut tx, &mut buf) {
                            return;
                        }
                        if let Some(reason) = eof {
                            let _ = send_blocking(&mut tx, BridgeMsg::Exited(reason));
                            return;
                        }
                    }
                    Ok(PtyMsg::Eof) => {
                        let _ = send_blocking(&mut tx, BridgeMsg::Exited("process exited".into()));
                        return;
                    }
                    Ok(PtyMsg::Error(e)) => {
                        let _ =
                            send_blocking(&mut tx, BridgeMsg::Exited(format!("pty error: {e}")));
                        return;
                    }
                    Err(_) => {
                        let _ = send_blocking(&mut tx, BridgeMsg::Exited("pty closed".into()));
                        return;
                    }
                }
            }
        });

    if let Err(e) = spawned {
        eprintln!("[ghostwave] pty bridge thread failed to start: {e}");
    }

    PtyRxSlot(Arc::new(Slot {
        tag: PaneTag(id),
        rx: Mutex::new(Some(out_rx)),
        keepalive: None,
    }))
}

/// The slot for a pane that has no child (a restored remote pane): its stream
/// stays open and yields nothing until the pane reconnects and takes a real one.
pub fn idle(id: crate::pane::PaneId) -> PtyRxSlot {
    let (tx, out_rx) = fmpsc::channel::<BridgeMsg>(COALESCE_QUEUE);
    PtyRxSlot(Arc::new(Slot {
        tag: PaneTag(id),
        rx: Mutex::new(Some(out_rx)),
        keepalive: Some(tx),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;
    use std::task::{Context, Poll, Waker};
    use std::time::{Duration, Instant};

    /// The byte at stream offset `offset`; the bridge must deliver the whole run
    /// unchanged and in order.
    fn byte_at(offset: usize) -> u8 {
        (offset % 251) as u8
    }

    fn chunk_at(start: usize, len: usize) -> Vec<u8> {
        (start..start + len).map(byte_at).collect()
    }

    /// Polls `stream` until `want` bytes have arrived, running the update loop's
    /// other work (the `between` closure — the stand-in for the key path) once after
    /// every message. Returns the bytes and the message count.
    fn drain_until(
        stream: &mut Pin<Box<dyn Stream<Item = Message> + Send>>,
        want: usize,
        between: &mut dyn FnMut(),
    ) -> (Vec<u8>, usize) {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut got: Vec<u8> = Vec::new();
        let mut messages = 0usize;
        while got.len() < want {
            match stream.as_mut().poll_next(&mut cx) {
                Poll::Ready(Some(Message::Output { data, .. })) => {
                    assert!(
                        data.len() <= COALESCE_CAP,
                        "one iced update must never carry more than the cap: {} bytes",
                        data.len()
                    );
                    got.extend_from_slice(&data);
                    messages += 1;
                    between();
                }
                Poll::Ready(Some(_)) => panic!("a data-only feed must not report an exit"),
                Poll::Ready(None) => {
                    panic!("the stream ended after {} of {want} bytes", got.len())
                }
                Poll::Pending => {
                    assert!(
                        Instant::now() < deadline,
                        "the burst never finished: {} of {want} bytes",
                        got.len()
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
        (got, messages)
    }

    #[test]
    fn a_replay_sized_burst_arrives_as_bounded_messages() {
        const TOTAL: usize = 1024 * 1024;
        const FIRST: usize = 96 * 1024; // one and a half caps: unsplittable input
        let (tx, rx) = sync_channel(512);
        let slot = spawn(crate::pane::PaneId(1), rx);

        let feeder = std::thread::spawn(move || {
            tx.send(PtyMsg::Data(chunk_at(0, FIRST)))
                .expect("the bridge takes the first chunk");
            let mut sent = FIRST;
            while sent < TOTAL {
                let len = (TOTAL - sent).min(4096);
                if tx.send(PtyMsg::Data(chunk_at(sent, len))).is_err() {
                    return sent;
                }
                sent += len;
            }
            sent
        });

        let mut keys = 0usize;
        let mut stream = stream(&slot);
        let (got, messages) = drain_until(&mut stream, TOTAL, &mut || keys += 1);

        assert_eq!(feeder.join().expect("the feeder finished"), TOTAL);
        assert_eq!(got, chunk_at(0, TOTAL), "every byte, in order, exactly once");
        assert_eq!(keys, messages, "the update loop ran between updates");
        assert!(
            messages >= TOTAL / COALESCE_CAP,
            "a replay is many updates, not one blob: {messages} messages for {TOTAL} bytes"
        );
    }

    #[test]
    fn a_full_queue_backpressures_the_bridge() {
        // The undrained capacity is the 64-message queue plus the feeder's own
        // channel plus the <32 KiB being coalesced; the small channel is what keeps
        // the total strictly below the feed, so the feeder cannot finish.
        const TOTAL: usize = COALESCE_QUEUE * COALESCE_CAP + 64 * 1024;
        let (tx, rx) = sync_channel(4);
        let slot = spawn(crate::pane::PaneId(2), rx);

        let (done_tx, done_rx) = mpsc::channel();
        let feeder = std::thread::spawn(move || {
            let mut sent = 0usize;
            while sent < TOTAL {
                let len = (TOTAL - sent).min(4096);
                if tx.send(PtyMsg::Data(chunk_at(sent, len))).is_err() {
                    break;
                }
                sent += len;
            }
            let _ = done_tx.send(sent);
        });

        // Nobody polls the stream, so the bridge fills the queue and then waits,
        // which backs up the reader channel and parks the feeder.
        assert!(
            done_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "the feeder cannot finish into a full queue"
        );

        let mut stream = stream(&slot);
        let (got, _) = drain_until(&mut stream, TOTAL, &mut || {});
        drop(stream);

        assert_eq!(
            done_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("the feeder finishes once the UI drains"),
            TOTAL
        );
        feeder.join().expect("the feeder finished");
        assert_eq!(got, chunk_at(0, TOTAL), "every byte, in order, exactly once");
    }
}
