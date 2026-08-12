//! Keybindings: zellij-style modes (pane/tab/resize/scroll/search/move/
//! session/locked) plus waveterm-style global binds.

use libghostty_vt::key::{Key, Mods};

use crate::layout::Dir;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Pane,
    Tab,
    Resize,
    Scroll,
    Search,
    EnterSearch,
    Move,
    Session,
    Locked,
    RenameTab,
    RenamePane,
    RenameSession,
}

impl Mode {
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Pane => "PANE",
            Mode::Tab => "TAB",
            Mode::Resize => "RESIZE",
            Mode::Scroll => "SCROLL",
            Mode::Search => "SEARCH",
            Mode::EnterSearch => "SEARCH>",
            Mode::Move => "MOVE",
            Mode::Session => "SESSION",
            Mode::Locked => "LOCKED",
            Mode::RenameTab => "RENAME TAB",
            Mode::RenamePane => "RENAME PANE",
            Mode::RenameSession => "RENAME SESSION",
        }
    }

    /// Keybind hints for the status bar.
    pub fn hints(&self) -> &'static str {
        match self {
            Mode::Normal => "Ctrl+p pane | Ctrl+t tab | Ctrl+n resize | Ctrl+s scroll | Ctrl+h move | Ctrl+o session | Ctrl+g lock | Ctrl+q quit",
            Mode::Pane => "n new | d down | r right | s stacked | x close | h/j/k/l focus | p cycle | f zoom | z frames | c rename | Esc exit",
            Mode::Tab => "n new | x close | h/l prev/next | r rename | b break | 1-9 goto | Tab last | Esc exit",
            Mode::Resize => "h/j/k/l resize | H/J/K/L shrink | =/+ grow | - shrink | Esc exit",
            Mode::Scroll => "j/k scroll | d/u half page | s search | Ctrl+c bottom | e edit | Esc exit",
            Mode::Search => "n/p next/prev | c case | w wrap | o whole word | Ctrl+s done | Esc exit",
            Mode::EnterSearch => "type to search | Enter done | Esc cancel",
            Mode::Move => "h/j/k/l move pane | n next | p previous | Esc exit",
            Mode::Session => "w session manager | d detach (save+quit) | Esc exit",
            Mode::Locked => "Ctrl+g unlock (input passes through)",
            Mode::RenameTab => "type new tab name | Enter confirm | Esc cancel",
            Mode::RenamePane => "type new pane name | Enter confirm | Esc cancel",
            Mode::RenameSession => "type new session name | Enter confirm | Esc cancel",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    // pane mode
    NewPane,
    NewPaneDown,
    NewPaneRight,
    NewPaneStacked,
    ClosePane,
    MoveFocus(Dir),
    SwitchFocus,
    ToggleZoom,
    ToggleFrames,
    RenamePane,
    CycleStack,
    // tab mode
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    GoToTab(u8),
    ToggleLastTab,
    RenameTab,
    BreakPane,
    // resize mode
    Resize(Dir, bool),
    ResizeAll(bool),
    // scroll mode
    ScrollUp(i32),
    ScrollDown(i32),
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
    ScrollToBottom,
    EditScrollback,
    // search
    SearchStart,
    SearchNext,
    SearchPrev,
    SearchToggleCase,
    SearchToggleWrap,
    SearchToggleWholeWord,
    ConfirmSearch,
    // move mode
    MovePane(Dir),
    MovePaneNext,
    MovePanePrev,
    // rename
    ConfirmRename,
    // generic
    ExitMode,
    // global
    Quit,
    Lock,
    Unlock,
    OpenCommandBar,
    OpenSearch,
    Copy,
    Paste,
    ClearTerminal,
    OpenFile(String),
    ConnectSsh(String),
    NewAi,
    // session mode
    SessionManager,
    SessionNew,
    SessionRename,
    SessionDelete,
    Detach,
    Theme(String),
    FocusPane(u8),
    MoveTab(Dir),
    Toast(&'static str),
}

/// What to do with a key press.
#[derive(Clone, Debug)]
pub enum Dispatch {
    Action(Action),
    /// Send to the active terminal pane.
    Pass,
    /// Ignore (modal mode, unbound key).
    Ignore,
    /// Printable character for text-input modes (rename/search).
    Char(char),
}

pub struct KeyPress {
    pub key: Key,
    pub mods: Mods,
    pub text: Option<char>,
}

fn mods_match(m: Mods, ctrl: bool, shift: bool, alt: bool, sup: bool) -> bool {
    let want = Mods::empty()
        | if ctrl { Mods::CTRL } else { Mods::empty() }
        | if shift { Mods::SHIFT } else { Mods::empty() }
        | if alt { Mods::ALT } else { Mods::empty() }
        | if sup { Mods::SUPER } else { Mods::empty() };
    let got = m & (Mods::CTRL | Mods::SHIFT | Mods::ALT | Mods::SUPER);
    got == want
}

fn ctrl(k: Key, m: Mods, key: Key) -> bool {
    k == key && mods_match(m, true, false, false, false)
}
fn ctrl_shift(k: Key, m: Mods, key: Key) -> bool {
    k == key && mods_match(m, true, true, false, false)
}
fn plain(k: Key, m: Mods, key: Key) -> bool {
    k == key && mods_match(m, false, false, false, false)
}
fn shift(k: Key, m: Mods, key: Key) -> bool {
    k == key && mods_match(m, false, true, false, false)
}
fn alt(k: Key, m: Mods, key: Key) -> bool {
    k == key && mods_match(m, false, false, true, false)
}

fn digit_key(k: Key) -> Option<u8> {
    use Key::*;
    Some(match k {
        Digit1 => 1,
        Digit2 => 2,
        Digit3 => 3,
        Digit4 => 4,
        Digit5 => 5,
        Digit6 => 6,
        Digit7 => 7,
        Digit8 => 8,
        Digit9 => 9,
        _ => return None,
    })
}

fn arrow_dir(k: Key) -> Option<Dir> {
    use Key::*;
    Some(match k {
        ArrowLeft => Dir::Left,
        ArrowRight => Dir::Right,
        ArrowUp => Dir::Up,
        ArrowDown => Dir::Down,
        _ => return None,
    })
}

fn hjkl_dir(k: Key) -> Option<Dir> {
    use Key::*;
    Some(match k {
        H => Dir::Left,
        J => Dir::Down,
        K => Dir::Up,
        L => Dir::Right,
        _ => return None,
    })
}

/// Global waveterm-style binds (active in most modes).
pub fn global_binds(k: Key, m: Mods) -> Option<Action> {
    use Key::*;
    Some(match k {
        _ if ctrl_shift(k, m, P) => Action::OpenCommandBar,
        _ if ctrl_shift(k, m, F) => Action::OpenSearch,
        _ if ctrl_shift(k, m, C) => Action::Copy,
        _ if ctrl_shift(k, m, V) => Action::Paste,
        _ if ctrl_shift(k, m, D) => Action::NewPaneDown,
        _ if ctrl(k, m, D) => Action::NewPaneRight,
        _ if ctrl_shift(k, m, W) => Action::CloseTab,
        _ if ctrl(k, m, W) => Action::ClosePane,
        _ if ctrl_shift(k, m, T) => Action::NewTab,
        _ if ctrl(k, m, M) => Action::ToggleZoom,
        _ if ctrl(k, m, K) => Action::ClearTerminal,
        _ if plain(k, m, F2) => Action::RenameTab,
        _ if ctrl(k, m, BracketLeft) => Action::PrevTab,
        _ if ctrl(k, m, BracketRight) => Action::NextTab,
        _ if ctrl(k, m, Tab) => Action::NextTab,
        _ if ctrl_shift(k, m, Tab) => Action::PrevTab,
        _ if ctrl(k, m, Digit0) => Action::FocusPane(9),
        _ if digit_key(k).is_some() && mods_match(m, true, false, false, false) => {
            Action::GoToTab(digit_key(k).unwrap())
        }
        _ if digit_key(k).is_some() && mods_match(m, true, true, false, false) => {
            Action::FocusPane(digit_key(k).unwrap().saturating_sub(1))
        }
        // waveterm: Ctrl+Shift+arrows / hjkl focus
        _ if arrow_dir(k).is_some() && mods_match(m, true, true, false, false) => {
            Action::MoveFocus(arrow_dir(k).unwrap())
        }
        _ if hjkl_dir(k).is_some() && mods_match(m, true, true, false, false) => {
            Action::MoveFocus(hjkl_dir(k).unwrap())
        }
        // zellij shared_except: Alt binds
        _ if alt(k, m, N) => Action::NewPane,
        _ if alt(k, m, H) => Action::MoveFocus(Dir::Left),
        _ if alt(k, m, L) => Action::MoveFocus(Dir::Right),
        _ if alt(k, m, J) => Action::MoveFocus(Dir::Down),
        _ if alt(k, m, K) => Action::MoveFocus(Dir::Up),
        _ if alt(k, m, I) => Action::MoveTab(Dir::Left),
        _ if alt(k, m, O) => Action::MoveTab(Dir::Right),
        _ if alt(k, m, Equal) => Action::ResizeAll(true),
        _ if alt(k, m, Minus) => Action::ResizeAll(false),
        // scroll helpers
        _ if shift(k, m, Home) => Action::ScrollToBottom,
        _ if shift(k, m, End) => Action::ScrollToBottom,
        _ if shift(k, m, PageUp) => Action::PageUp,
        _ if shift(k, m, PageDown) => Action::PageDown,
        _ => return None,
    })
}

/// Zellij mode-entry binds (Ctrl+p/t/n/s/o/h/g). Returns the mode to enter.
pub fn entry_bind(k: Key, m: Mods) -> Option<Mode> {
    use Key::*;
    Some(match k {
        _ if ctrl(k, m, P) => Mode::Pane,
        _ if ctrl(k, m, T) => Mode::Tab,
        _ if ctrl(k, m, N) => Mode::Resize,
        _ if ctrl(k, m, S) => Mode::Scroll,
        _ if ctrl(k, m, O) => Mode::Session,
        _ if ctrl(k, m, H) => Mode::Move,
        _ if ctrl(k, m, G) => Mode::Locked,
        _ => return None,
    })
}

/// Main dispatch. `mode` is the current mode; returns what to do.
pub fn handle(mode: Mode, kp: &KeyPress) -> Dispatch {
    let k = kp.key;
    let m = kp.mods;

    // Locked mode: everything passes except Ctrl+g.
    if mode == Mode::Locked {
        if ctrl(k, m, Key::G) {
            return Dispatch::Action(Action::Unlock);
        }
        return Dispatch::Pass;
    }

    // Rename modes: text capture.
    if mode == Mode::RenameTab || mode == Mode::RenamePane || mode == Mode::RenameSession {
        if kp.key == Key::Escape || ctrl(k, m, Key::C) {
            return Dispatch::Action(Action::ExitMode);
        }
        if kp.key == Key::Enter {
            return Dispatch::Action(Action::ConfirmRename);
        }
        if kp.key == Key::Backspace {
            return Dispatch::Action(Action::Toast("backspace"));
        }
        if let Some(c) = kp.text {
            return Dispatch::Char(c);
        }
        return Dispatch::Ignore;
    }

    // EnterSearch: text capture.
    if mode == Mode::EnterSearch {
        if kp.key == Key::Escape || ctrl(k, m, Key::C) {
            return Dispatch::Action(Action::ExitMode);
        }
        if kp.key == Key::Enter {
            return Dispatch::Action(Action::ConfirmSearch);
        }
        if kp.key == Key::Backspace {
            return Dispatch::Action(Action::Toast("backspace"));
        }
        if let Some(c) = kp.text {
            return Dispatch::Char(c);
        }
        return Dispatch::Ignore;
    }

    // Global waveterm binds.
    if let Some(action) = global_binds(k, m) {
        return Dispatch::Action(action);
    }

    // Esc exits any modal mode back to Normal (zellij behavior). Locked
    // mode is handled above and stays Ctrl+g-only on purpose.
    if kp.key == Key::Escape && mode != Mode::Normal {
        return Dispatch::Action(Action::ExitMode);
    }

    // Mode-specific binds.
    let action = match mode {
        Mode::Normal => normal_binds(k, m),
        Mode::Pane => pane_binds(k, m),
        Mode::Tab => tab_binds(k, m),
        Mode::Resize => resize_binds(k, m),
        Mode::Scroll => scroll_binds(k, m),
        Mode::Search => search_binds(k, m),
        Mode::Move => move_binds(k, m),
        Mode::Session => session_binds(k, m),
        Mode::EnterSearch | Mode::RenameTab | Mode::RenamePane | Mode::RenameSession | Mode::Locked => {
            return Dispatch::Ignore
        }
    };

    match action {
        Some(a) => Dispatch::Action(a),
        None => match mode {
            // Normal mode: everything not bound goes to the terminal.
            Mode::Normal => Dispatch::Pass,
            _ => Dispatch::Ignore,
        },
    }
}

/// Ctrl+q quit.
pub fn is_quit(k: Key, m: Mods) -> bool {
    ctrl(k, m, Key::Q)
}

fn normal_binds(k: Key, m: Mods) -> Option<Action> {
    let _ = (k, m);
    None // zellij normal mode has no binds; everything passes
}

fn pane_binds(k: Key, m: Mods) -> Option<Action> {
    use Key::*;
    let dir = arrow_dir(k).or_else(|| hjkl_dir(k));
    Some(match k {
        _ if plain(k, m, N) => Action::NewPane,
        _ if plain(k, m, D) => Action::NewPaneDown,
        _ if plain(k, m, R) => Action::NewPaneRight,
        _ if plain(k, m, S) => Action::NewPaneStacked,
        _ if plain(k, m, X) => Action::ClosePane,
        _ if plain(k, m, P) => Action::SwitchFocus,
        _ if plain(k, m, F) => Action::ToggleZoom,
        _ if plain(k, m, Z) => Action::ToggleFrames,
        _ if plain(k, m, W) => Action::CycleStack,
        _ if plain(k, m, C) => Action::RenamePane,
        _ if plain(k, m, E) => Action::Toast("embed/floating not implemented"),
        _ if let Some(d) = dir => Action::MoveFocus(d),
        _ => return None,
    })
}

fn tab_binds(k: Key, m: Mods) -> Option<Action> {
    use Key::*;
    let dir = arrow_dir(k).or_else(|| hjkl_dir(k));
    Some(match k {
        _ if plain(k, m, N) => Action::NewTab,
        _ if plain(k, m, X) => Action::CloseTab,
        _ if plain(k, m, R) => Action::RenameTab,
        _ if plain(k, m, B) => Action::BreakPane,
        _ if plain(k, m, S) => Action::Toast("active-sync not implemented"),
        _ if plain(k, m, BracketLeft) => Action::Toast("break-pane-left not implemented"),
        _ if plain(k, m, BracketRight) => Action::Toast("break-pane-right not implemented"),
        _ if plain(k, m, Tab) => Action::ToggleLastTab,
        _ if digit_key(k).is_some() => Action::GoToTab(digit_key(k).unwrap()),
        _ if let Some(d) = dir => match d {
            Dir::Left | Dir::Up => Action::PrevTab,
            Dir::Right | Dir::Down => Action::NextTab,
        },
        _ => return None,
    })
}

fn resize_binds(k: Key, m: Mods) -> Option<Action> {
    use Key::*;
    let dir = arrow_dir(k).or_else(|| hjkl_dir(k));
    // Capital letters (shift) shrink.
    let shift_dir = |d: Dir| match d {
        Dir::Left => Dir::Left,
        Dir::Right => Dir::Right,
        Dir::Up => Dir::Up,
        Dir::Down => Dir::Down,
    };
    let _ = shift_dir;
    Some(match k {
        _ if plain(k, m, H) => Action::Resize(Dir::Left, true),
        _ if plain(k, m, J) => Action::Resize(Dir::Down, true),
        _ if plain(k, m, K) => Action::Resize(Dir::Up, true),
        _ if plain(k, m, L) => Action::Resize(Dir::Right, true),
        _ if shift(k, m, H) => Action::Resize(Dir::Left, false),
        _ if shift(k, m, J) => Action::Resize(Dir::Down, false),
        _ if shift(k, m, K) => Action::Resize(Dir::Up, false),
        _ if shift(k, m, L) => Action::Resize(Dir::Right, false),
        _ if plain(k, m, Equal) || plain(k, m, Minus) => Action::ResizeAll(plain(k, m, Equal)),
        _ if let Some(d) = dir => Action::Resize(d, true),
        _ => return None,
    })
}

fn scroll_binds(k: Key, m: Mods) -> Option<Action> {
    use Key::*;
    let dir = arrow_dir(k).or_else(|| hjkl_dir(k));
    Some(match k {
        _ if plain(k, m, S) => Action::SearchStart,
        _ if plain(k, m, E) => Action::EditScrollback,
        _ if ctrl(k, m, C) => Action::ScrollToBottom,
        _ if plain(k, m, J) || k == ArrowDown => Action::ScrollDown(1),
        _ if plain(k, m, K) || k == ArrowUp => Action::ScrollUp(1),
        _ if ctrl(k, m, F) || k == PageDown || k == ArrowRight || k == Key::L => Action::PageDown,
        _ if ctrl(k, m, B) || k == PageUp || k == ArrowLeft || k == Key::H => Action::PageUp,
        _ if plain(k, m, D) => Action::HalfPageDown,
        _ if plain(k, m, U) => Action::HalfPageUp,
        _ => return None,
    })
    .or_else(|| dir.map(|_| Action::ScrollToBottom))
}

fn search_binds(k: Key, m: Mods) -> Option<Action> {
    use Key::*;
    Some(match k {
        _ if ctrl(k, m, S) => Action::ExitMode,
        _ if ctrl(k, m, C) => Action::ScrollToBottom,
        _ if plain(k, m, N) => Action::SearchNext,
        _ if plain(k, m, P) => Action::SearchPrev,
        _ if plain(k, m, C) => Action::SearchToggleCase,
        _ if plain(k, m, W) => Action::SearchToggleWrap,
        _ if plain(k, m, O) => Action::SearchToggleWholeWord,
        _ if plain(k, m, J) || k == ArrowDown => Action::ScrollDown(1),
        _ if plain(k, m, K) || k == ArrowUp => Action::ScrollUp(1),
        _ if ctrl(k, m, F) || k == PageDown => Action::PageDown,
        _ if ctrl(k, m, B) || k == PageUp => Action::PageUp,
        _ if plain(k, m, D) => Action::HalfPageDown,
        _ if plain(k, m, U) => Action::HalfPageUp,
        _ => return None,
    })
}

fn move_binds(k: Key, m: Mods) -> Option<Action> {
    use Key::*;
    let dir = arrow_dir(k).or_else(|| hjkl_dir(k));
    Some(match k {
        _ if plain(k, m, N) || k == Tab => Action::MovePaneNext,
        _ if plain(k, m, P) => Action::MovePanePrev,
        _ if let Some(d) = dir => Action::MovePane(d),
        _ => return None,
    })
}

fn session_binds(k: Key, m: Mods) -> Option<Action> {
    use Key::*;
    Some(match k {
        // zellij-style: w = session manager, d = detach.
        _ if plain(k, m, W) => Action::SessionManager,
        _ if plain(k, m, D) => Action::Detach,
        _ => return None,
    })
}

/// Map a macroquad KeyCode + mods + text into a ghostty KeyPress.
pub fn from_macroquad(
    code: macroquad::prelude::KeyCode,
    text: Option<char>,
) -> Option<KeyPress> {
    use macroquad::prelude::KeyCode as Q;
    use macroquad::prelude::*;

    let key = match code {
        Q::A => Key::A,
        Q::B => Key::B,
        Q::C => Key::C,
        Q::D => Key::D,
        Q::E => Key::E,
        Q::F => Key::F,
        Q::G => Key::G,
        Q::H => Key::H,
        Q::I => Key::I,
        Q::J => Key::J,
        Q::K => Key::K,
        Q::L => Key::L,
        Q::M => Key::M,
        Q::N => Key::N,
        Q::O => Key::O,
        Q::P => Key::P,
        Q::Q => Key::Q,
        Q::R => Key::R,
        Q::S => Key::S,
        Q::T => Key::T,
        Q::U => Key::U,
        Q::V => Key::V,
        Q::W => Key::W,
        Q::X => Key::X,
        Q::Y => Key::Y,
        Q::Z => Key::Z,
        Q::Key0 => Key::Digit0,
        Q::Key1 => Key::Digit1,
        Q::Key2 => Key::Digit2,
        Q::Key3 => Key::Digit3,
        Q::Key4 => Key::Digit4,
        Q::Key5 => Key::Digit5,
        Q::Key6 => Key::Digit6,
        Q::Key7 => Key::Digit7,
        Q::Key8 => Key::Digit8,
        Q::Key9 => Key::Digit9,
        Q::Space => Key::Space,
        Q::Enter => Key::Enter,
        Q::Tab => Key::Tab,
        Q::Backspace => Key::Backspace,
        Q::Delete => Key::Delete,
        Q::Escape => Key::Escape,
        Q::Up => Key::ArrowUp,
        Q::Down => Key::ArrowDown,
        Q::Left => Key::ArrowLeft,
        Q::Right => Key::ArrowRight,
        Q::Home => Key::Home,
        Q::End => Key::End,
        Q::PageUp => Key::PageUp,
        Q::PageDown => Key::PageDown,
        Q::Insert => Key::Insert,
        Q::F1 => Key::F1,
        Q::F2 => Key::F2,
        Q::F3 => Key::F3,
        Q::F4 => Key::F4,
        Q::F5 => Key::F5,
        Q::F6 => Key::F6,
        Q::F7 => Key::F7,
        Q::F8 => Key::F8,
        Q::F9 => Key::F9,
        Q::F10 => Key::F10,
        Q::F11 => Key::F11,
        Q::F12 => Key::F12,
        Q::Minus => Key::Minus,
        Q::Equal => Key::Equal,
        Q::LeftBracket => Key::BracketLeft,
        Q::RightBracket => Key::BracketRight,
        Q::Backslash => Key::Backslash,
        Q::Semicolon => Key::Semicolon,
        Q::Apostrophe => Key::Quote,
        Q::Comma => Key::Comma,
        Q::Period => Key::Period,
        Q::Slash => Key::Slash,
        Q::GraveAccent => Key::Backquote,
        Q::LeftControl => Key::ControlLeft,
        Q::RightControl => Key::ControlRight,
        Q::LeftShift => Key::ShiftLeft,
        Q::RightShift => Key::ShiftRight,
        Q::LeftAlt => Key::AltLeft,
        Q::RightAlt => Key::AltRight,
        Q::LeftSuper => Key::MetaLeft,
        Q::RightSuper => Key::MetaRight,
        Q::CapsLock => Key::CapsLock,
        Q::NumLock => Key::NumLock,
        Q::ScrollLock => Key::ScrollLock,
        Q::PrintScreen => Key::PrintScreen,
        Q::Pause => Key::Pause,
        Q::Menu => Key::ContextMenu,
        Q::Kp0 => Key::Numpad0,
        Q::Kp1 => Key::Numpad1,
        Q::Kp2 => Key::Numpad2,
        Q::Kp3 => Key::Numpad3,
        Q::Kp4 => Key::Numpad4,
        Q::Kp5 => Key::Numpad5,
        Q::Kp6 => Key::Numpad6,
        Q::Kp7 => Key::Numpad7,
        Q::Kp8 => Key::Numpad8,
        Q::Kp9 => Key::Numpad9,
        Q::KpAdd => Key::NumpadAdd,
        Q::KpSubtract => Key::NumpadSubtract,
        Q::KpMultiply => Key::NumpadMultiply,
        Q::KpDivide => Key::NumpadDivide,
        Q::KpEnter => Key::NumpadEnter,
        Q::KpDecimal => Key::NumpadDecimal,
        Q::KpEqual => Key::NumpadEqual,
        _ => return None,
    };

    let mods = current_mods();

    Some(KeyPress { key, mods, text })
}

/// Current modifier state from macroquad.
pub fn current_mods() -> Mods {
    use macroquad::prelude::*;
    let mut m = Mods::empty();
    if is_key_down(KeyCode::LeftShift) || is_key_down(KeyCode::RightShift) {
        m |= Mods::SHIFT;
    }
    if is_key_down(KeyCode::LeftAlt) || is_key_down(KeyCode::RightAlt) {
        m |= Mods::ALT;
    }
    if is_key_down(KeyCode::LeftControl) || is_key_down(KeyCode::RightControl) {
        m |= Mods::CTRL;
    }
    if is_key_down(KeyCode::LeftSuper) || is_key_down(KeyCode::RightSuper) {
        m |= Mods::SUPER;
    }
    m
}

/// Unshifted codepoint for a key (Kitty protocol needs it).
pub fn unshifted_codepoint(key: Key) -> char {
    use Key::*;
    match key {
        A => 'a', B => 'b', C => 'c', D => 'd', E => 'e', F => 'f', G => 'g', H => 'h',
        I => 'i', J => 'j', K => 'k', L => 'l', M => 'm', N => 'n', O => 'o', P => 'p',
        Q => 'q', R => 'r', S => 's', T => 't', U => 'u', V => 'v', W => 'w', X => 'x',
        Y => 'y', Z => 'z',
        Digit0 => '0', Digit1 => '1', Digit2 => '2', Digit3 => '3', Digit4 => '4',
        Digit5 => '5', Digit6 => '6', Digit7 => '7', Digit8 => '8', Digit9 => '9',
        Space => ' ',
        Minus => '-', Equal => '=', BracketLeft => '[', BracketRight => ']',
        Backslash => '\\', Semicolon => ';', Quote => '\'', Comma => ',', Period => '.',
        Slash => '/', Backquote => '`',
        _ => '\0',
    }
}
