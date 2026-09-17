//! ghostwave on iced: a tab of terminal panes laid out by iced's `pane_grid`,
//! painted on one clipped canvas, with pane frames, labels, click-to-focus and
//! wheel scrollback.

use ghostwave::config::Config;
use iced::{
    keyboard,
    widget::pane_grid,
    Element, Size, Subscription, Task,
};
use libghostty_vt::key;

mod app;
mod chrome;
mod config;
mod fonts;
#[allow(dead_code)]
mod grid_session;
mod input;
mod keys;
mod measure;
mod pane;
mod pane_canvas;
mod panes_grid;
mod pty_bridge;
mod tick;

use app::AppState;
use pane::SharedPane;
use pane_canvas::ChromeData;

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
    /// Carries the new window size; every pane's grid follows from it.
    Resized(Size),
    /// Advances the wave and expires the toast; subscribed only while something
    /// is actually animating.
    Tick,
    /// Focuses the tab whose chip was clicked in the tab bar.
    FocusTab(usize),
    /// Focuses a pane by its tab-local index.
    FocusPane(usize),
    /// A pane-local mouse event: forwarded into the application (when it tracks
    /// the mouse) or into selection / viewport scrolling.
    Mouse {
        tab: usize,
        pane: usize,
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
    /// The pane-grid layer's split drag: the split's new ratio, in `[0, 1]`.
    GridResize {
        split: pane_grid::Split,
        ratio: f32,
    },
    /// The pane-grid layer's pane drag: picked, dropped or cancelled.
    GridDrag(pane_grid::DragEvent),
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
        if self.state.iced_config.dynamic_title != crate::config::DynamicTitleMode::Off {
            let tab = self.state.current();
            if let Some(pane) = tab.pane(tab.active) {
                let guard = pane.lock().unwrap();
                let title = guard.cached_title();
                if !title.is_empty() {
                    return format!("{title} — ghostwave");
                }
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
            Message::FocusPane(index) => self.state.focus(index),
            Message::Mouse {
                tab,
                pane,
                ev,
                mods,
            } => {
                if let Some(pane_ref) = self.state.pane_mut(tab, pane) {
                    let mut guard = pane_ref.lock().unwrap();
                    let (w, h) = guard
                        .last_bounds
                        .map(|b| (b.width, b.height))
                        .unwrap_or_else(|| {
                            (
                                (guard.term.cols as u32 * self.state.cell.w) as f32,
                                (guard.term.rows as u32 * self.state.cell.h) as f32,
                            )
                        });
                    let rect = ghostwave::layout::Rect {
                        x: 0.0,
                        y: 0.0,
                        w,
                        h,
                    };
                    guard.mouse_event(
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
            // The pane-grid layer's own interactions: the grid keeps whatever
            // ratio a split drag ends on, and a drop re-arranges its cells.
            Message::GridResize { split, ratio } => self.state.grid_resize(split, ratio),
            Message::GridDrag(event) => self.state.grid_drag(event),
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
        // Everything the chrome needs, owned: the layers are rebuilt every view.
        let chrome = ChromeData {
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
        };

        panes_grid::view(&self.state, panes, mouse_tracking, chrome)
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
