//! ghostwave on iced — M1: a tab of panes from `ghostwave::layout`, painted on
//! one clipped canvas, with pane frames, labels, click-to-focus and wheel
//! scrollback.
//!
//! Everything terminal-shaped lives in the app crate (`ghostwave::term::TermPane`:
//! pty, VT, scrollback, selection, key encoding, pane log) along with config,
//! theme and the layout tree. This crate owns only what iced needs: the canvas
//! painter, the event plumbing, and the bridge from each pane's pty reader into
//! iced's executor. iced 0.14's reactive renderer is the point of the port — it
//! redraws on state change, never on a timer.

use ghostwave::config::Config;
use ghostwave::layout::SplitDir;
use iced::{
    keyboard,
    widget::canvas::Canvas,
    Element, Fill, Point, Rectangle, Size, Subscription, Task,
};
use libghostty_vt::key;

mod app;
mod chrome;
mod fonts;
mod keys;
mod measure;
mod pane;
mod pane_canvas;
mod pty_bridge;
mod tick;

use app::AppState;
use pane::SharedPane;
use pane_canvas::{ChromeData, TerminalCanvas};

/// Same face the macroquad build loads as its primary font.
const FONT_BYTES: &[u8] = include_bytes!("../../assets/JetBrainsMono-Medium.ttf");

/// Size the window asks for before the compositor has its say (the first
/// `resize_events` message replaces it).
const START_WINDOW: Size = Size::new(1200.0, 800.0);

#[derive(Debug)]
pub enum Message {
    /// Coalesced pty output from a pane's reader thread.
    Output {
        tab: usize,
        pane: usize,
        data: Vec<u8>,
    },
    /// A pane's child exited, or its pty errored out.
    Exited {
        tab: usize,
        pane: usize,
        reason: String,
    },
    /// The window changed size: re-derive every pane's rect and grid.
    Resized(Size),
    /// Animation tick: advances the wave and expires the toast. Only subscribed
    /// while something is actually animating (`app::TickRate`).
    Tick,
    /// A click on a tab chip in the tab bar.
    FocusTab(usize),
    /// A click landed in a pane.
    FocusPane { tab: usize, pane: usize },
    /// Wheel over a pane: negative scrolls into history.
    Scroll {
        tab: usize,
        pane: usize,
        lines: i32,
    },
    /// A key press, translated to the VT encoder by `keys::keypress`.
    KeyDown {
        key: keyboard::Key,
        physical: keyboard::key::Physical,
        modifiers: keyboard::Modifiers,
        text: Option<String>,
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
            Message::Output { tab, pane, data } => {
                if let Some(pane) = self.state.pane_mut(tab, pane) {
                    pane.lock().unwrap().ingest(&data);
                }
                // Output in flight energizes the wave on the next tick.
                self.state.pty_activity = true;
            }
            Message::Exited { tab, pane, reason } => {
                if let Some(pane) = self.state.pane_mut(tab, pane) {
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
            Message::FocusPane { tab, pane } => {
                if tab == self.state.active_tab {
                    self.state.focus(pane);
                }
            }
            Message::Scroll { tab, pane, lines } => {
                if let Some(pane) = self.state.pane_mut(tab, pane) {
                    pane.lock().unwrap().scroll(lines);
                }
            }
            Message::KeyDown {
                key,
                physical,
                modifiers,
                text,
            } => {
                // TEMPORARY until M3: the app's pane-mode binds (Ctrl+p d/e/x)
                // arrive with the keymap modes and Action dispatch, so this is a
                // stand-in so multi-pane is reachable at all. Alt+Shift+d/e =
                // split, Alt+Shift+w = close, Alt+arrows = move focus.
                if self.temporary_bind(&physical, modifiers) {
                    return Task::none();
                }
                // Any key energizes the wave (`app.rs:2153-2156`).
                self.state.bump_wave();
                if let Some(kp) = keys::keypress(&key, physical, modifiers, text.as_deref()) {
                    if let Some(pane) = self.state.active_pane() {
                        pane.lock().unwrap().key_press(&kp, key::Action::Press);
                    }
                }
            }
        }
        Task::none()
    }

    /// See the note at the call site: this exists only so M1 is usable.
    fn temporary_bind(
        &mut self,
        physical: &keyboard::key::Physical,
        modifiers: keyboard::Modifiers,
    ) -> bool {
        use ghostwave::layout::Dir;
        use keyboard::key::{Code, Physical};

        let alt = modifiers.contains(keyboard::Modifiers::ALT);
        if !alt {
            return false;
        }
        let Physical::Code(code) = physical else {
            return false;
        };
        let shift = modifiers.contains(keyboard::Modifiers::SHIFT);
        match (*code, shift) {
            (Code::KeyD, true) => self.state.split(SplitDir::Horizontal).is_ok(),
            (Code::KeyE, true) => self.state.split(SplitDir::Vertical).is_ok(),
            (Code::KeyW, true) => {
                self.state.close_active();
                true
            }
            (Code::ArrowLeft, false) => {
                self.state.focus_dir(Dir::Left);
                true
            }
            (Code::ArrowRight, false) => {
                self.state.focus_dir(Dir::Right);
                true
            }
            (Code::ArrowUp, false) => {
                self.state.focus_dir(Dir::Up);
                true
            }
            (Code::ArrowDown, false) => {
                self.state.focus_dir(Dir::Down);
                true
            }
            _ => false,
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let tab = self.state.current();
        let panes: Vec<(usize, SharedPane)> = tab
            .panes
            .iter()
            .enumerate()
            .filter_map(|(id, pane)| pane.as_ref().map(|p| (id, p.clone())))
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
                // The reference displays `config.theme` verbatim, even when
                // `theme::by_name` fell back to another palette for an unknown
                // name; kept as-is for parity.
                theme_name: self.state.config.theme.clone(),
                sync_panes: tab.sync_panes,
                wave: self.state.config.visual_wave.then_some(chrome::WaveParams {
                    energy: self.state.wave_energy,
                    phase: self.state.wave_phase,
                    amplitude: self.state.config.wave_amplitude,
                }),
                toast: self.state.toast.as_ref().map(|(m, _)| m.clone()),
            },
        })
        .width(Fill)
        .height(Fill)
        .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            // The canvas never captures keyboard events, so presses arrive as
            // plain runtime events and this is the whole input path.
            iced::event::listen_with(key_listener),
            iced::window::resize_events().map(|(_id, size)| Message::Resized(size)),
        ];
        // Animation. iced redraws on state change only, so the wave (and a toast
        // expiring) need an explicit tick; while nothing animates there is none,
        // which is the whole point of the port. Rates mirror the macroquad loop's
        // tiers: ~60 FPS active, 8 FPS with the wave idling.
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

/// Key presses are the only events this front-end listens for. Mouse input goes
/// through the canvas (`pane_canvas::TerminalCanvas`), which knows the rects.
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
            ..
        }) => Some(Message::KeyDown {
            key,
            physical: physical_key,
            modifiers,
            text: text.map(|t| t.to_string()),
        }),
        _ => None,
    }
}

/// Unused directly but kept so the pane type is in scope for readers; the canvas
/// owns painting.
fn main() -> iced::Result {
    let mut app = iced::application(App::new, App::update, App::view)
        .title(App::title)
        .subscription(App::subscription)
        .window_size(START_WINDOW)
        // iced builds its font system with an EMPTY database plus its own Fira
        // Sans, so nothing resolves beyond those (that is why box-drawing,
        // braille, nerd and emoji glyphs were tofu). Register the face the cell
        // metrics come from — the canvas draws with it by name — plus the same
        // fallback chain the macroquad build discovers with fc-list.
        .font(fonts::PRIMARY_BYTES);
    for (_family, bytes) in fonts::fallback_bytes(12) {
        app = app.font(bytes);
    }
    app.run()
}
