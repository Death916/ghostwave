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
            Mode::Pane => "n new | d down | r right | s stacked | x close | h/j/k/l focus | p cycle | f float | e hide | E cycle float | z frames | c rename | Esc exit",
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
    ToggleFloating,
    ToggleHideFloat,
    CycleFloat,
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
    ConnectDurableSsh(String),
    KillRemoteSession,
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
    ToggleSyncPanes,
    ToggleScratchpad,
    Ghostwave,
    Backspace,
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
        _ if plain(k, m, F12) => Action::ToggleScratchpad,
        _ if ctrl(k, m, Backquote) => Action::ToggleScratchpad,
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
        _ if alt(k, m, F) => Action::ToggleZoom, // Zellij: Alt+f toggles pane fullscreen within window
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
            return Dispatch::Action(Action::Backspace);
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
            return Dispatch::Action(Action::Backspace);
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
        _ if plain(k, m, F) => Action::ToggleFloating,
        _ if plain(k, m, E) => Action::ToggleHideFloat,
        _ if shift(k, m, E) => Action::CycleFloat,
        _ if plain(k, m, Z) => Action::ToggleZoom, // Zellij: Ctrl+p z toggles fullscreen/zoom
        _ if shift(k, m, Z) => Action::ToggleFrames,
        _ if plain(k, m, W) => Action::CycleStack,
        _ if plain(k, m, C) => Action::RenamePane,
        _ if plain(k, m, B) => Action::ToggleSyncPanes,
        _ if shift(k, m, D) => Action::ToggleScratchpad,
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
        _ if plain(k, m, S) => Action::ToggleSyncPanes,
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

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::key::{Key, Mods};

    fn kp(key: Key, mods: Mods) -> KeyPress { KeyPress { key, mods, text: None } }
    fn kp_char(key: Key, mods: Mods, c: char) -> KeyPress { KeyPress { key, mods, text: Some(c) } }
    fn ctrl_mod() -> Mods { Mods::CTRL }
    fn no_mod() -> Mods { Mods::empty() }

    #[test]
    fn locked_passes_and_only_ctrl_g_exits() {
        assert!(matches!(handle(Mode::Locked, &kp(Key::A, no_mod())), Dispatch::Pass));
        assert!(matches!(handle(Mode::Locked, &kp(Key::Enter, no_mod())), Dispatch::Pass));
        assert!(matches!(handle(Mode::Locked, &kp(Key::G, ctrl_mod())), Dispatch::Action(Action::Unlock)));
        // Ctrl+Shift+G is not unlock
        assert!(matches!(handle(Mode::Locked, &kp(Key::G, Mods::CTRL | Mods::SHIFT)), Dispatch::Pass));
    }

    #[test]
    fn normal_unbound_passes_to_terminal() {
        assert!(matches!(handle(Mode::Normal, &kp(Key::A, no_mod())), Dispatch::Pass));
        assert!(matches!(handle(Mode::Normal, &kp(Key::Enter, no_mod())), Dispatch::Pass));
    }

    #[test]
    fn esc_exits_modal_but_not_locked_or_normal() {
        assert!(matches!(handle(Mode::Pane, &kp(Key::Escape, no_mod())), Dispatch::Action(Action::ExitMode)));
        assert!(matches!(handle(Mode::Tab, &kp(Key::Escape, no_mod())), Dispatch::Action(Action::ExitMode)));
        assert!(matches!(handle(Mode::Locked, &kp(Key::Escape, no_mod())), Dispatch::Pass), "Esc in Locked should pass, not exit");
        assert!(!matches!(handle(Mode::Normal, &kp(Key::Escape, no_mod())), Dispatch::Action(Action::ExitMode)));
    }

    #[test]
    fn is_quit_only_ctrl_q() {
        assert!(is_quit(Key::Q, Mods::CTRL));
        assert!(!is_quit(Key::Q, Mods::empty()));
        assert!(!is_quit(Key::Q, Mods::CTRL | Mods::SHIFT));
        assert!(!is_quit(Key::W, Mods::CTRL));
    }

    #[test]
    fn entry_binds_for_all_modes() {
        assert_eq!(entry_bind(Key::P, Mods::CTRL), Some(Mode::Pane));
        assert_eq!(entry_bind(Key::T, Mods::CTRL), Some(Mode::Tab));
        assert_eq!(entry_bind(Key::N, Mods::CTRL), Some(Mode::Resize));
        assert_eq!(entry_bind(Key::S, Mods::CTRL), Some(Mode::Scroll));
        assert_eq!(entry_bind(Key::O, Mods::CTRL), Some(Mode::Session));
        assert_eq!(entry_bind(Key::H, Mods::CTRL), Some(Mode::Move));
        assert_eq!(entry_bind(Key::G, Mods::CTRL), Some(Mode::Locked));
        assert_eq!(entry_bind(Key::A, Mods::CTRL), None);
        assert_eq!(entry_bind(Key::P, Mods::empty()), None);
    }

    #[test]
    fn global_binds_ctrl_shift_p_is_command_bar() {
        assert!(matches!(global_binds(Key::P, Mods::CTRL | Mods::SHIFT), Some(Action::OpenCommandBar)));
        assert!(matches!(global_binds(Key::C, Mods::CTRL | Mods::SHIFT), Some(Action::Copy)));
        assert!(matches!(global_binds(Key::V, Mods::CTRL | Mods::SHIFT), Some(Action::Paste)));
        assert_eq!(global_binds(Key::A, Mods::empty()), None);
    }

    #[test]
    fn pane_binds_basic() {
        assert!(matches!(handle(Mode::Pane, &kp(Key::N, no_mod())), Dispatch::Action(Action::NewPane)));
        assert!(matches!(handle(Mode::Pane, &kp(Key::X, no_mod())), Dispatch::Action(Action::ClosePane)));
        assert!(matches!(handle(Mode::Pane, &kp(Key::H, no_mod())), Dispatch::Action(Action::MoveFocus(Dir::Left))));
        // unbound in pane mode is Ignore, not Pass
        assert!(matches!(handle(Mode::Pane, &kp(Key::A, no_mod())), Dispatch::Ignore));
    }

    #[test]
    fn tab_binds_goto_and_new() {
        assert!(matches!(handle(Mode::Tab, &kp(Key::N, no_mod())), Dispatch::Action(Action::NewTab)));
        assert!(matches!(handle(Mode::Tab, &kp(Key::Digit1, no_mod())), Dispatch::Action(Action::GoToTab(1))));
        assert!(matches!(handle(Mode::Tab, &kp(Key::Digit9, no_mod())), Dispatch::Action(Action::GoToTab(9))));
    }

    #[test]
    fn rename_modes_capture_text_and_confirm() {
        assert!(matches!(handle(Mode::RenameTab, &kp_char(Key::A, no_mod(), 'a')), Dispatch::Char('a')));
        assert!(matches!(handle(Mode::RenameTab, &kp(Key::Enter, no_mod())), Dispatch::Action(Action::ConfirmRename)));
        assert!(matches!(handle(Mode::RenameTab, &kp(Key::Escape, no_mod())), Dispatch::Action(Action::ExitMode)));
        assert!(matches!(handle(Mode::RenameTab, &kp(Key::Backspace, no_mod())), Dispatch::Action(Action::Backspace)));
        // non-char, non-special is Ignore
        assert!(matches!(handle(Mode::RenameTab, &kp(Key::F1, no_mod())), Dispatch::Ignore));
    }

    #[test]
    fn enter_search_captures_and_confirms() {
        assert!(matches!(handle(Mode::EnterSearch, &kp_char(Key::A, no_mod(), 'x')), Dispatch::Char('x')));
        assert!(matches!(handle(Mode::EnterSearch, &kp(Key::Enter, no_mod())), Dispatch::Action(Action::ConfirmSearch)));
        assert!(matches!(handle(Mode::EnterSearch, &kp(Key::Escape, no_mod())), Dispatch::Action(Action::ExitMode)));
        assert!(matches!(handle(Mode::EnterSearch, &kp(Key::Backspace, no_mod())), Dispatch::Action(Action::Backspace)));
    }

    #[test]
    fn session_binds() {
        assert!(matches!(handle(Mode::Session, &kp(Key::W, no_mod())), Dispatch::Action(Action::SessionManager)));
        assert!(matches!(handle(Mode::Session, &kp(Key::D, no_mod())), Dispatch::Action(Action::Detach)));
        assert!(matches!(handle(Mode::Session, &kp(Key::A, no_mod())), Dispatch::Ignore));
    }

    #[test]
    fn unshifted_codepoint_letters_and_digits() {
        assert_eq!(unshifted_codepoint(Key::A), 'a');
        assert_eq!(unshifted_codepoint(Key::Z), 'z');
        assert_eq!(unshifted_codepoint(Key::Digit0), '0');
        assert_eq!(unshifted_codepoint(Key::Minus), '-');
        assert_eq!(unshifted_codepoint(Key::F1), '\0');
    }

    #[test]
    fn mode_labels_non_empty() {
        for m in [Mode::Normal, Mode::Pane, Mode::Tab, Mode::Resize, Mode::Scroll, Mode::Search, Mode::EnterSearch, Mode::Move, Mode::Session, Mode::Locked, Mode::RenameTab] {
            assert!(!m.label().is_empty());
            assert!(!m.hints().is_empty());
        }
    }
}
