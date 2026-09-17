//! Periodic animation tick.
//!
//! iced has no `time::every` in 0.14 — its own time module only exposes `now()`,
//! so the tick is a thread that publishes a message on a fixed period, exactly
//! like the pty bridges do for output. A subscription's identity is its `Hash`,
//! and [`TickSlot`] hashes by rate, so changing the rate (wave energized ↔ idling
//! ↔ nothing animating) tears the old ticker down and starts a new one; when iced
//! drops the receiver the send fails and the thread exits by itself.

use std::pin::Pin;
use std::time::Duration;

use iced::futures::channel::mpsc as fmpsc;
use iced::futures::Stream;

use crate::Message;
use crate::app::TickRate;

/// Subscription payload: which period to tick at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TickSlot {
    pub rate: TickRate,
}

impl TickSlot {
    fn period(self) -> Duration {
        match self.rate {
            // ~60 FPS while the wave is energized or a toast is on screen.
            TickRate::Fast => Duration::from_millis(16),
            // 8 FPS while the wave idles, matching the macroquad idle tier.
            TickRate::Slow => Duration::from_millis(125),
            // Nothing animating: iced should not be woken at all.
            TickRate::Off => Duration::from_secs(3600),
        }
    }
}

/// Start a ticker for this rate and stream its messages.
pub fn stream(slot: &TickSlot) -> Pin<Box<dyn Stream<Item = Message> + Send>> {
    let period = slot.period();
    let (tx, rx) = fmpsc::unbounded::<Message>();

    let spawned = std::thread::Builder::new()
        .name("ghost-iced-tick".into())
        .spawn(move || loop {
            std::thread::sleep(period);
            if tx.unbounded_send(Message::Tick).is_err() {
                // Subscription dropped: stop.
                return;
            }
        });

    if let Err(e) = spawned {
        eprintln!("[ghost-iced] tick thread failed to start: {e}");
        return Box::pin(iced::futures::stream::pending::<Message>());
    }

    Box::pin(rx)
}
