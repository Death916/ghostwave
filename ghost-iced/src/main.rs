//! ghostwave on iced: a tab of panes from `ghostwave::layout`, painted on one
//! clipped canvas, with pane frames, labels, click-to-focus and wheel
//! scrollback.

use ghostwave::config::Config;
use iced::{
    keyboard,
    widget::canvas::Canvas,
    Element, Fill, Point, Rectangle, Size, Subscription, Task,
};
use libghostty_vt::key;

mod app;
mod chrome;
mod fonts;
mod input;
mod keys;
mod measure;
mod pane;
mod pane_canvas;
mod pty_bridge;
mod tick;

use app::AppState;
use pane::SharedPane;
use pane_canvas::{ChromeData, TerminalCanvas};

/// Cell metrics are measured from it and the canvas draws text with it.
const FONT_BYTES: &[u8] = include_bytes!("../../assets/JetBrainsMono-Medium.ttf");

/// Replaced by the first `resize_events` message.
const START_WINDOW: Size = Size::new(1200.0, 800.0);

/// Events the iced runtime delivers, plus the mouse events the canvas synthesizes.
#[derive(Debug)]
pub enum Message {
    /// Carries pty output already coalesced by the pane's reader thread.
    Output {
        /// The pane's own identity, not its position — see [`pane::PaneId`].
        id: pane::PaneId,
        data: Vec<u8>,
    },
    /// Signals that a pane's child exited or its pty errored out.
    Exited {
        id: pane::PaneId,
        reason: String,
    },
    /// Carries the new window size; every pane's rect and grid is re-derived from it.
    Resized(Size),
    /// Advances the wave and expires the toast; subscribed only while something
    /// is actually animating.
    Tick,
    /// Focuses the tab whose chip was clicked in the tab bar.
    FocusTab(usize),
    /// A pane-local mouse event: the canvas resolved which pane it landed in. The
    /// pane's own mouse encoder then decides whether it goes to the application
    /// (when it tracks the mouse) or into selection / viewport scrolling.
    Mouse {
        tab: usize,
        pane: usize,
        /// Focus this pane before feeding it the event.
        focus: bool,
        ev: ghostwave::term::MouseEv,
        /// Modifier state for the pane's encoder; iced's mouse events do not
        /// carry it.
        mods: libghostty_vt::key::Mods,
    },
    /// Dispatches a key press through [`keys::keypress`] and then
    /// [`input::key_press`], or [`input::repeat`] when the platform repeated it.
    KeyDown {
        key: keyboard::Key,
        physical: keyboard::key::Physical,
        modifiers: keyboard::Modifiers,
        text: Option<String>,
        /// The platform generated this one because the key is held down.
        repeat: bool,
    },
    /// Matters only to a pane: the kitty keyboard protocol lets an application ask
    /// for release events, and a modal bind must not leak its release into the shell.
    KeyUp {
        key: keyboard::Key,
        physical: keyboard::key::Physical,
        modifiers: keyboard::Modifiers,
    },
}

struct App {
    state: AppState,
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let config = Config::load();
        let cell = pane::cell_dims(FONT_BYTES, config.font_size, config.row_gap);
        let state = AppState::new(config, cell, START_WINDOW).expect("spawn the first pane");
        eprintln!(
            "[ghost-iced] cell {}x{} px, baseline {}, theme {}, frames {}, tab '{}' with {} pane(s)",
            cell.w,
            cell.h,
            cell.baseline,
            state.theme.name,
            state.show_frames,
            state.current().name,
            state.current().panes.iter().filter(|p| p.is_some()).count()
        );
        (Self { state }, Task::none())
    }

    fn title(&self) -> String {
        let tab = self.state.current();
        if let Some(pane) = tab.pane(tab.active) {
            let title = pane.lock().unwrap().term.title();
            if !title.is_empty() {
                return title;
            }
        }
        "ghostwave (iced)".to_string()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Output { id, data } => {
                if let Some(pane) = self.state.pane_by_id(id) {
                    pane.lock().unwrap().ingest(&data);
                    // Output in flight energizes the wave on the next tick.
                    self.state.pty_activity = true;
                }
            }
            Message::Exited { id, reason } => {
                if let Some(pane) = self.state.pane_by_id(id) {
                    pane.lock().unwrap().exited(&reason);
                }
            }
            Message::Resized(size) => self.state.set_window(size),
            Message::Tick => self.state.tick(),
            Message::FocusTab(index) => {
                if index < self.state.tabs.len() {
                    self.state.active_tab = index;
                    self.state.relayout();
                }
            }
            Message::Mouse {
                tab,
                pane,
                focus,
                ev,
                mods,
            } => {
                if focus && tab == self.state.active_tab {
                    self.state.focus(pane);
                }
                let rect = self
                    .state
                    .rects
                    .iter()
                    .find(|(id, _)| *id == pane)
                    .map(|(_, r)| ghostwave::layout::Rect {
                        x: r.x,
                        y: r.y,
                        w: r.width,
                        h: r.height,
                    });
                if let (Some(rect), Some(pane)) = (rect, self.state.pane_mut(tab, pane)) {
                    pane.lock().unwrap().mouse_event(
                        rect,
                        self.state.cell,
                        self.state.config.padding,
                        ev,
                        mods,
                    );
                }
            }
            Message::KeyDown {
                key,
                physical,
                modifiers,
                text,
                repeat,
            } => {
                // Remembered for the mouse path: iced's mouse events carry no
                // modifiers.
                keys::note_modifiers(modifiers);
                if let Some(kp) = keys::keypress(&key, physical, modifiers, text.as_deref()) {
                    if repeat {
                        input::repeat(&mut self.state, kp);
                    } else {
                        input::key_press(&mut self.state, kp, key::Action::Press);
                    }
                }
            }
            Message::KeyUp {
                key,
                physical,
                modifiers,
            } => {
                keys::note_modifiers(modifiers);
                if let Some(kp) = keys::keypress(&key, physical, modifiers, None) {
                    input::key_release(&mut self.state, kp);
                }
            }
        }
        if self.state.quit {
            // `Ctrl+q` / `Action::Quit`.
            return iced::exit();
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let tab = self.state.current();
        let panes: Vec<(usize, SharedPane)> = tab
            .panes
            .iter()
            .enumerate()
            .filter_map(|(id, pane)| pane.as_ref().map(|p| (id, p.clone())))
            .collect();
        let mouse_tracking: Vec<usize> = panes
            .iter()
            .filter(|(_, pane)| pane.lock().unwrap().mouse_tracking())
            .map(|(id, _)| *id)
            .collect();

        Canvas::new(TerminalCanvas {
            tab: self.state.active_tab,
            panes,
            rects: self.state.rects.clone(),
            active: tab.active,
            cell: self.state.cell,
            font_px: self.state.config.font_size,
            padding: self.state.config.padding,
            show_frames: self.state.show_frames,
            theme: self.state.theme,
            content: {
                let c = self.state.content();
                Rectangle::new(
                    Point::new(c.x, c.y),
                    Size::new(c.w.max(1.0), c.h.max(1.0)),
                )
            },
            chrome: ChromeData {
                tabs: self
                    .state
                    .tabs
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (t.label(i), i == self.state.active_tab))
                    .collect(),
                mode_label: self.state.mode.label().to_string(),
                hints: self.state.mode.hints().to_string(),
                // The configured theme name, not the resolved palette's.
                theme_name: self.state.config.theme.clone(),
                sync_panes: tab.sync_panes,
                wave: self.state.config.visual_wave.then_some(chrome::WaveParams {
                    energy: self.state.wave_energy,
                    phase: self.state.wave_phase,
                    amplitude: self.state.config.wave_amplitude,
                }),
                toast: self.state.toast.as_ref().map(|(m, _)| m.clone()),
                rename: match self.state.mode {
                    ghostwave::keymap::Mode::RenameTab => {
                        Some(("Tab".to_string(), self.state.rename_buf.clone()))
                    }
                    ghostwave::keymap::Mode::RenamePane => {
                        Some(("Pane".to_string(), self.state.rename_buf.clone()))
                    }
                    _ => None,
                },
            },
            mouse_tracking,
        })
        .width(Fill)
        .height(Fill)
        .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            iced::event::listen_with(key_listener),
            iced::window::resize_events().map(|(_id, size)| Message::Resized(size)),
        ];
        // The wave and an expiring toast need an explicit tick; while nothing
        // animates there is none.
        match self.state.tick_rate() {
            app::TickRate::Off => {}
            rate => subs.push(Subscription::run_with(
                tick::TickSlot { rate },
                tick::stream,
            )),
        }
        // One stream per live pane; each carries its own (tab, pane) tag.
        for tab in &self.state.tabs {
            for pane in tab.panes.iter().flatten() {
                let slot = { pane.lock().unwrap().rx.clone() };
                subs.push(Subscription::run_with(slot, pty_bridge::stream));
            }
        }
        Subscription::batch(subs)
    }
}

/// Key events are the only runtime events this front-end listens for. Mouse input
/// goes through the canvas ([`pane_canvas::TerminalCanvas`]), which knows the rects.
fn key_listener(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key,
            physical_key,
            modifiers,
            text,
            repeat,
            ..
        }) => Some(Message::KeyDown {
            key,
            physical: physical_key,
            modifiers,
            text: text.map(|t| t.to_string()),
            repeat,
        }),
        iced::Event::Keyboard(keyboard::Event::KeyReleased {
            key,
            physical_key,
            modifiers,
            ..
        }) => Some(Message::KeyUp {
            key,
            physical: physical_key,
            modifiers,
        }),
        // A modifier change is not a message of its own: the mouse path reads the
        // current state from `keys`.
        iced::Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
            keys::note_modifiers(modifiers);
            None
        }
        _ => None,
    }
}

fn main() -> iced::Result {
    let mut app = iced::application(App::new, App::update, App::view)
        .title(App::title)
        .subscription(App::subscription)
        .window_size(START_WINDOW)
        // iced's font system starts from an empty database plus its own Fira
        // Sans, so every face has to be registered: the primary one (the canvas
        // draws with it by name) plus the system fallbacks.
        .font(fonts::PRIMARY_BYTES);
    for (_family, bytes) in fonts::fallback_bytes(12) {
        app = app.font(bytes);
    }
    app.run()
}
