//! Periodic animation tick: publishes a [`Message::Tick`] on a fixed period.
//!
//! [`TickSlot`] hashes by rate, so a change of rate starts a new ticker, and the
//! thread exits when iced drops the receiver.

use std::pin::Pin;
use std::time::Duration;

use iced::futures::channel::mpsc as fmpsc;
use iced::futures::Stream;

use crate::Message;
use crate::app::TickRate;

/// The period a ticker runs at, as a `Hash`able subscription identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TickSlot {
    pub rate: TickRate,
}

impl TickSlot {
    fn period(self) -> Duration {
        match self.rate {
            TickRate::Fast => Duration::from_millis(16),
            TickRate::Slow => Duration::from_millis(125),
            TickRate::Off => Duration::from_secs(3600),
        }
    }
}

/// Starts a ticker thread for `slot` and streams its [`Message::Tick`]s.
pub fn stream(slot: &TickSlot) -> Pin<Box<dyn Stream<Item = Message> + Send>> {
    let period = slot.period();
    let (tx, rx) = fmpsc::unbounded::<Message>();

    let spawned = std::thread::Builder::new()
        .name("ghost-iced-tick".into())
        .spawn(move || loop {
            std::thread::sleep(period);
            if tx.unbounded_send(Message::Tick).is_err() {
                return;
            }
        });

    if let Err(e) = spawned {
        eprintln!("[ghostwave] tick thread failed to start: {e}");
        return Box::pin(iced::futures::stream::pending::<Message>());
    }

    Box::pin(rx)
}
