//! iced keyboard events -> the app crate's VT key encoding.
//!
//! `libghostty_vt::key::Key` is a W3C key list and iced reports the *physical*
//! key as a W3C code too, so this module is the mapping table between the two.

use ghostwave::keymap::KeyPress;
use libghostty_vt::key::{Key, Mods};

use std::sync::atomic::{AtomicU8, Ordering};

use iced::keyboard;
use keyboard::key::Code as C;

/// Encodes an iced key press as the app crate's [`KeyPress`].
///
/// `None` when the press carries no VT-representable key: an unidentified
/// physical key, a modifier-only press, or a W3C code that has no
/// `libghostty_vt::key::Key` counterpart (see [`code_to_key`]).
pub fn keypress(
    key: &keyboard::Key,
    physical: keyboard::key::Physical,
    modifiers: keyboard::Modifiers,
    text: Option<&str>,
) -> Option<KeyPress> {
    let code = match physical {
        keyboard::key::Physical::Code(code) => code,
        keyboard::key::Physical::Unidentified(_) => return None,
    };

    let key_code = code_to_key(code)?;

    // The platform's text wins, but only when it is exactly one printable
    // character; a control character (Ctrl+C hands over `\u{3}`) or a composed
    // sequence is dropped rather than truncated, and the encoder reconstructs
    // the intended sequence from the key + mods. Only when iced reported no
    // text at all do we fall back to the logical character of the key.
    let text = match text {
        Some(text) => single_printable_char(text),
        None => logical_char(key),
    };

    Some(KeyPress {
        key: key_code,
        mods: mods_from(modifiers),
        text,
    })
}

/// `None` for codes with no `Key` counterpart *and* for modifier-only keys,
/// which the encoder never sees as key events: it derives the modifier state
/// from [`Mods`] instead.
fn code_to_key(code: C) -> Option<Key> {
    Some(match code {
        C::Backquote => Key::Backquote,
        C::Backslash => Key::Backslash,
        C::BracketLeft => Key::BracketLeft,
        C::BracketRight => Key::BracketRight,
        C::Comma => Key::Comma,
        C::Digit0 => Key::Digit0,
        C::Digit1 => Key::Digit1,
        C::Digit2 => Key::Digit2,
        C::Digit3 => Key::Digit3,
        C::Digit4 => Key::Digit4,
        C::Digit5 => Key::Digit5,
        C::Digit6 => Key::Digit6,
        C::Digit7 => Key::Digit7,
        C::Digit8 => Key::Digit8,
        C::Digit9 => Key::Digit9,
        C::Equal => Key::Equal,
        C::IntlBackslash => Key::IntlBackslash,
        C::IntlRo => Key::IntlRo,
        C::IntlYen => Key::IntlYen,
        C::Minus => Key::Minus,
        C::Period => Key::Period,
        C::Quote => Key::Quote,
        C::Semicolon => Key::Semicolon,
        C::Slash => Key::Slash,

        C::Backspace => Key::Backspace,
        C::CapsLock => Key::CapsLock,
        C::ContextMenu => Key::ContextMenu,
        C::Enter => Key::Enter,
        C::Space => Key::Space,
        C::Tab => Key::Tab,
        C::Convert => Key::Convert,
        C::KanaMode => Key::KanaMode,
        C::NonConvert => Key::NonConvert,

        C::Delete => Key::Delete,
        C::End => Key::End,
        C::Help => Key::Help,
        C::Home => Key::Home,
        C::Insert => Key::Insert,
        C::PageDown => Key::PageDown,
        C::PageUp => Key::PageUp,
        C::ArrowDown => Key::ArrowDown,
        C::ArrowLeft => Key::ArrowLeft,
        C::ArrowRight => Key::ArrowRight,
        C::ArrowUp => Key::ArrowUp,

        C::NumLock => Key::NumLock,
        C::Numpad0 => Key::Numpad0,
        C::Numpad1 => Key::Numpad1,
        C::Numpad2 => Key::Numpad2,
        C::Numpad3 => Key::Numpad3,
        C::Numpad4 => Key::Numpad4,
        C::Numpad5 => Key::Numpad5,
        C::Numpad6 => Key::Numpad6,
        C::Numpad7 => Key::Numpad7,
        C::Numpad8 => Key::Numpad8,
        C::Numpad9 => Key::Numpad9,
        C::NumpadAdd => Key::NumpadAdd,
        C::NumpadBackspace => Key::NumpadBackspace,
        C::NumpadClear => Key::NumpadClear,
        C::NumpadClearEntry => Key::NumpadClearEntry,
        C::NumpadComma => Key::NumpadComma,
        C::NumpadDecimal => Key::NumpadDecimal,
        C::NumpadDivide => Key::NumpadDivide,
        C::NumpadEnter => Key::NumpadEnter,
        C::NumpadEqual => Key::NumpadEqual,
        C::NumpadMemoryAdd => Key::NumpadMemoryAdd,
        C::NumpadMemoryClear => Key::NumpadMemoryClear,
        C::NumpadMemoryRecall => Key::NumpadMemoryRecall,
        C::NumpadMemoryStore => Key::NumpadMemoryStore,
        C::NumpadMemorySubtract => Key::NumpadMemorySubtract,
        C::NumpadMultiply => Key::NumpadMultiply,
        C::NumpadParenLeft => Key::NumpadParenLeft,
        C::NumpadParenRight => Key::NumpadParenRight,
        C::NumpadSubtract => Key::NumpadSubtract,

        C::Escape => Key::Escape,
        C::F1 => Key::F1,
        C::F2 => Key::F2,
        C::F3 => Key::F3,
        C::F4 => Key::F4,
        C::F5 => Key::F5,
        C::F6 => Key::F6,
        C::F7 => Key::F7,
        C::F8 => Key::F8,
        C::F9 => Key::F9,
        C::F10 => Key::F10,
        C::F11 => Key::F11,
        C::F12 => Key::F12,
        C::F13 => Key::F13,
        C::F14 => Key::F14,
        C::F15 => Key::F15,
        C::F16 => Key::F16,
        C::F17 => Key::F17,
        C::F18 => Key::F18,
        C::F19 => Key::F19,
        C::F20 => Key::F20,
        C::F21 => Key::F21,
        C::F22 => Key::F22,
        C::F23 => Key::F23,
        C::F24 => Key::F24,
        C::F25 => Key::F25,
        C::Fn => Key::Fn,
        C::FnLock => Key::FnLock,
        C::PrintScreen => Key::PrintScreen,
        C::ScrollLock => Key::ScrollLock,
        C::Pause => Key::Pause,

        C::BrowserBack => Key::BrowserBack,
        C::BrowserFavorites => Key::BrowserFavorites,
        C::BrowserForward => Key::BrowserForward,
        C::BrowserHome => Key::BrowserHome,
        C::BrowserRefresh => Key::BrowserRefresh,
        C::BrowserSearch => Key::BrowserSearch,
        C::BrowserStop => Key::BrowserStop,
        C::Eject => Key::Eject,
        C::LaunchApp1 => Key::LaunchApp1,
        C::LaunchApp2 => Key::LaunchApp2,
        C::LaunchMail => Key::LaunchMail,
        C::MediaPlayPause => Key::MediaPlayPause,
        C::MediaSelect => Key::MediaSelect,
        C::MediaStop => Key::MediaStop,
        C::MediaTrackNext => Key::MediaTrackNext,
        C::MediaTrackPrevious => Key::MediaTrackPrevious,
        C::Power => Key::Power,
        C::Sleep => Key::Sleep,
        C::AudioVolumeDown => Key::AudioVolumeDown,
        C::AudioVolumeMute => Key::AudioVolumeMute,
        C::AudioVolumeUp => Key::AudioVolumeUp,
        C::WakeUp => Key::WakeUp,
        C::Copy => Key::Copy,
        C::Cut => Key::Cut,
        C::Paste => Key::Paste,
        C::KeyA => Key::A,
        C::KeyB => Key::B,
        C::KeyC => Key::C,
        C::KeyD => Key::D,
        C::KeyE => Key::E,
        C::KeyF => Key::F,
        C::KeyG => Key::G,
        C::KeyH => Key::H,
        C::KeyI => Key::I,
        C::KeyJ => Key::J,
        C::KeyK => Key::K,
        C::KeyL => Key::L,
        C::KeyM => Key::M,
        C::KeyN => Key::N,
        C::KeyO => Key::O,
        C::KeyP => Key::P,
        C::KeyQ => Key::Q,
        C::KeyR => Key::R,
        C::KeyS => Key::S,
        C::KeyT => Key::T,
        C::KeyU => Key::U,
        C::KeyV => Key::V,
        C::KeyW => Key::W,
        C::KeyX => Key::X,
        C::KeyY => Key::Y,
        C::KeyZ => Key::Z,
        // Modifier-only keys: tracked through `Mods`, never sent as key events.
        C::AltLeft
        | C::AltRight
        | C::ControlLeft
        | C::ControlRight
        | C::ShiftLeft
        | C::ShiftRight
        | C::SuperLeft
        | C::SuperRight => return None,

        C::F26 | C::F27 | C::F28 | C::F29 | C::F30 | C::F31 | C::F32 | C::F33 | C::F34 | C::F35 => {
            return None
        }
        C::Hiragana | C::Katakana | C::Lang1 | C::Lang2 | C::Lang3 | C::Lang4 | C::Lang5 => {
            return None
        }
        C::Abort
        | C::Again
        | C::Find
        | C::Open
        | C::Props
        | C::Resume
        | C::Select
        | C::Suspend
        | C::Undo
        | C::Meta
        | C::Hyper
        | C::Turbo
        | C::NumpadHash
        | C::NumpadStar => return None,

        // `Code` is `#[non_exhaustive]`: a code a future iced adds lands here.
        _ => return None,
    })
}

/// iced calls the Windows/Command key `LOGO`; ghostty calls it `SUPER`.
fn mods_from(modifiers: keyboard::Modifiers) -> Mods {
    use keyboard::Modifiers as M;
    let mut m = Mods::empty();
    if modifiers.contains(M::SHIFT) {
        m |= Mods::SHIFT;
    }
    if modifiers.contains(M::CTRL) {
        m |= Mods::CTRL;
    }
    if modifiers.contains(M::ALT) {
        m |= Mods::ALT;
    }
    if modifiers.contains(M::LOGO) {
        m |= Mods::SUPER;
    }
    m
}

/// iced hands over the produced text as a `&str`; a composed sequence (a
/// dead-key pair, an IME pre-edit run) is more than one `char` and is dropped
/// here rather than truncated into something the user did not type.
fn single_printable_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_none() && !c.is_control() {
        Some(c)
    } else {
        None
    }
}

fn logical_char(key: &keyboard::Key) -> Option<char> {
    match key {
        keyboard::Key::Character(s) => single_printable_char(s.as_ref()),
        keyboard::Key::Named(_) | keyboard::Key::Unidentified => None,
    }
}

/// iced's mouse events do not repeat the modifiers, but `TermPane::handle_mouse`
/// needs them: `selection_allowed` uses them to decide whether a drag selects
/// text or goes to the application (Shift bypasses application mouse tracking).
static MODS: AtomicU8 = AtomicU8::new(0);

const SHIFT_B: u8 = 1 << 0;
const CTRL_B: u8 = 1 << 1;
const ALT_B: u8 = 1 << 2;
const LOGO_B: u8 = 1 << 3;

/// Records the newest modifier state, because iced's mouse events do not carry it.
pub fn note_modifiers(modifiers: keyboard::Modifiers) {
    use keyboard::Modifiers as M;
    let mut bits = 0;
    if modifiers.contains(M::SHIFT) {
        bits |= SHIFT_B;
    }
    if modifiers.contains(M::CTRL) {
        bits |= CTRL_B;
    }
    if modifiers.contains(M::ALT) {
        bits |= ALT_B;
    }
    if modifiers.contains(M::LOGO) {
        bits |= LOGO_B;
    }
    MODS.store(bits, Ordering::Relaxed);
}

/// Returns the recorded modifiers as the app crate's key encoder wants them.
pub fn current_mods() -> Mods {
    let bits = MODS.load(Ordering::Relaxed);
    let mut m = Mods::empty();
    if bits & SHIFT_B != 0 {
        m |= Mods::SHIFT;
    }
    if bits & CTRL_B != 0 {
        m |= Mods::CTRL;
    }
    if bits & ALT_B != 0 {
        m |= Mods::ALT;
    }
    if bits & LOGO_B != 0 {
        m |= Mods::SUPER;
    }
    m
}
