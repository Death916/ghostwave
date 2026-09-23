//! Periodic session autosave: publishes a [`Message::Autosave`] on a fixed
//! period, on its own thread, independently of the animation tick
//! ([`crate::tick`]).

use std::pin::Pin;
use std::time::Duration;

use iced::futures::channel::mpsc as fmpsc;
use iced::futures::Stream;

use crate::Message;

/// How often the session file is written.
const PERIOD: Duration = Duration::from_secs(2);

/// The autosave subscription's identity, so iced keeps one publisher for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Autosave;

/// Starts an autosave thread for `slot` and streams its [`Message::Autosave`]s.
pub fn stream(slot: &Autosave) -> Pin<Box<dyn Stream<Item = Message> + Send>> {
    let _ = slot;
    let (tx, rx) = fmpsc::unbounded::<Message>();

    let spawned = std::thread::Builder::new()
        .name("ghost-iced-autosave".into())
        .spawn(move || loop {
            std::thread::sleep(PERIOD);
            if tx.unbounded_send(Message::Autosave).is_err() {
                return;
            }
        });

    if let Err(e) = spawned {
        eprintln!("[ghostwave] autosave thread failed to start: {e}");
        return Box::pin(iced::futures::stream::pending::<Message>());
    }

    Box::pin(rx)
}
