//! OSC 50 — xterm's "Change/Query Font", answered above the VT.
//!
//! A program asks the terminal what font it is using with
//! `ESC ] 50 ; ? ST` (or `BEL` for `ST`) and expects
//! `ESC ] 50 ; <font name> <same terminator>` back
//! (<https://terminalguide.namepad.de/seq/osc-50_xterm/>). The pinned VT engine
//! parses no OSC 50 at all, so nothing answers it today and nothing collides with
//! this module: the scanner only *reads* the inbound bytes, and the bytes the VT
//! receives are unchanged.
//!
//! [`Scanner`] is the whole protocol. It is fed the raw pty stream chunk by
//! chunk and returns zero or more complete replies to write back to the pty.
//!
//! # Grammar accepted
//!
//! A query is exactly `ESC ] 50 ; ?` terminated by `BEL` or `ESC \`. Everything
//! else is ignored:
//!
//! - `50;<font>` is xterm's *set* command, not a query — ignored (changing the
//!   font at runtime is out of scope).
//! - any other OSC (`12;?`, `4;0;?`, `52;…`, `0;title`) is ignored.
//! - a body longer than [`MAX_BODY`] abandons the sequence, so an OSC 8
//!   hyperlink or a clipboard payload is never buffered.
//!
//! # Fragmenting
//!
//! Scanner state survives across [`Scanner::feed`] calls: the pty reader hands
//! over arbitrary chunks, and a query split across two of them still gets
//! exactly one reply.
//!
//! # Replies are not escaped
//!
//! The family name is echoed verbatim. Family names from `fc-list` do not
//! normally contain `;`, and one that did would make the reply ambiguous for a
//! strict parser — a known limit, not a hidden one.

/// `ESC` — starts the OSC, and with `\` ends it.
const ESC: u8 = 0x1b;
/// `BEL` — the alternative terminator.
const BEL: u8 = 0x07;

/// The only body that is a query: `50;?`.
const QUERY_BODY: &[u8] = b"50;?";

/// Longest body retained, in bytes. `50;?` is four; anything longer is not a
/// font query and is abandoned rather than buffered.
pub const MAX_BODY: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// Not inside an escape sequence.
    Idle,
    /// Saw `ESC`, waiting for `]`.
    Esc,
    /// Inside an OSC body.
    Body,
    /// Saw `ESC` inside a body, waiting for the `\` of `ST`.
    BodyEsc,
}

/// Finds font queries in a pty byte stream and encodes their replies.
pub struct Scanner {
    state: State,
    buf: [u8; MAX_BODY],
    len: usize,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    /// A scanner that has seen no bytes yet.
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            buf: [0; MAX_BODY],
            len: 0,
        }
    }

    /// Consumes one chunk of pty output and returns the replies to send back.
    ///
    /// `family` is used verbatim as the answer to a query. The returned bytes
    /// are complete sequences (each with its own terminator) suitable for
    /// writing to the pty as-is; the returned vector is empty when the chunk
    /// contained no query.
    pub fn feed(&mut self, data: &[u8], family: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for &b in data {
            match self.state {
                State::Idle => {
                    if b == ESC {
                        self.state = State::Esc;
                    }
                }
                State::Esc => {
                    if b == b']' {
                        self.state = State::Body;
                        self.len = 0;
                    } else {
                        self.state = State::Idle;
                    }
                }
                State::Body => match b {
                    BEL => {
                        self.finish(BEL, family, &mut out);
                    }
                    ESC => self.state = State::BodyEsc,
                    _ => {
                        if self.len == MAX_BODY {
                            // Not a font query; drop it without buffering more.
                            self.state = State::Idle;
                        } else {
                            self.buf[self.len] = b;
                            self.len += 1;
                        }
                    }
                },
                State::BodyEsc => {
                    if b == b'\\' {
                        self.finish(ESC, family, &mut out);
                    } else {
                        // A stray ESC inside a body is not a valid command.
                        self.state = State::Idle;
                    }
                }
            }
        }
        out
    }

    /// Ends the current body: appends the reply when it was the font query,
    /// carries `esc` forward as the terminator that has to follow it, and
    /// returns the scanner to idle.
    fn finish(&mut self, esc: u8, family: &str, out: &mut Vec<u8>) {
        if &self.buf[..self.len] == QUERY_BODY {
            out.extend_from_slice(b"\x1b]50;");
            out.extend_from_slice(family.as_bytes());
            out.push(esc);
            if esc == ESC {
                out.push(b'\\');
            }
        }
        self.state = State::Idle;
        self.len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(family: &str, terminator: &[u8]) -> Vec<u8> {
        let mut v = b"\x1b]50;".to_vec();
        v.extend_from_slice(family.as_bytes());
        v.extend_from_slice(terminator);
        v
    }

    #[test]
    fn answers_the_font_query() {
        let mut s = Scanner::new();
        assert_eq!(
            s.feed(b"\x1b]50;?\x1b\\", "Fira Code"),
            reply("Fira Code", b"\x1b\\")
        );
    }

    #[test]
    fn echoes_bel_terminator() {
        let mut s = Scanner::new();
        let out = s.feed(b"\x1b]50;?\x07", "Fira Code");
        assert_eq!(out, reply("Fira Code", b"\x07"));
        assert!(
            out.ends_with(&[BEL]),
            "reply should end with BEL, got {out:?}"
        );
    }

    #[test]
    fn split_across_feeds_answers_once() {
        let mut s = Scanner::new();
        assert!(s.feed(b"\x1b]50", "Fira Code").is_empty());
        assert_eq!(
            s.feed(b";?\x1b\\", "Fira Code"),
            reply("Fira Code", b"\x1b\\")
        );
        assert!(s.feed(b"", "Fira Code").is_empty(), "no second reply");
    }

    #[test]
    fn ignores_set_command() {
        let mut s = Scanner::new();
        assert!(s.feed(b"\x1b]50;Monospace\x1b\\", "Fira Code").is_empty());
    }

    #[test]
    fn ignores_other_oscs() {
        let mut s = Scanner::new();
        assert!(s.feed(b"\x1b]12;?\x1b\\", "Fira Code").is_empty());
        assert!(s.feed(b"\x1b]4;0;?\x1b\\", "Fira Code").is_empty());
        assert!(s.feed(b"\x1b]0;a title\x07", "Fira Code").is_empty());
    }

    #[test]
    fn oversized_body_is_abandoned() {
        let mut s = Scanner::new();
        let mut stream = b"\x1b]50;".to_vec();
        stream.extend(std::iter::repeat_n(b'x', 200));
        stream.extend_from_slice(b"\x1b\\");
        assert!(s.feed(&stream, "Fira Code").is_empty());
        // The oversized sequence must not have left the scanner wedged.
        assert_eq!(
            s.feed(b"\x1b]50;?\x1b\\", "Fira Code"),
            reply("Fira Code", b"\x1b\\")
        );
    }
}
