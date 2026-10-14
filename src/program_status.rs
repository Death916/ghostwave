//! OSC 7501 — the Program Status Protocol, consumer side (parse, track, answer).
//!
//! A program writes `ESC ] 7501 ; key=value[:key=value…] ST` (or `BEL` for `ST`)
//! to its own pty to say what it is doing; the terminal keeps one record per id
//! and shows the picture however it likes. This module is the whole protocol:
//! [`Scanner`] finds sequences in a pty byte stream, [`Table`] holds the records,
//! and [`Tracker`] wires the two together with the lifetimes and the feature
//! detection reply.
//!
//! # Spec revision
//!
//! Pinned to **revision 0.4 (2026-10-10)** of the specification, which is the
//! live revision at `https://www.superlogical.com/rex/docs/build/program-status`.
//! Three deltas from revision 0.2 (2026-10-06) matter here and are all additive
//! or clarifying — the parsing grammar, the record table, the states, the keys
//! and every limit are byte-for-byte the same text in 0.2 and 0.4:
//!
//! - 0.3: absent `progress` means *indeterminate* (already true of the key table).
//! - 0.3: a program MAY send reports without querying first.
//! - 0.4: the feature-detection reply MAY carry `states=` and `kinds=` lists;
//!   0.4 also *removes* the terminfo capability (`Pst`) that 0.2 defined, so no
//!   terminfo entry is shipped for this protocol.
//!
//! The reply is therefore `ESC ] 7501 ; ?:states=…:kinds=… ST` — legal under 0.2
//! as well, because 0.2 asked only that the query be answered and a querying
//! program reads a reply, not a fixed byte string.
//!
//! # Everything a report contains is untrusted
//!
//! `msg` and `title` are decoded here and stored as text; they are never markup,
//! and the UI must show them as plain text ([`sanitize`] disarms the invisible
//! formatting and text-direction characters the grid tolerates). Nothing in a
//! record is ever written back to a program: the only bytes this module can
//! produce are [`QUERY_REPLY`], a constant.

use std::time::{Duration, Instant};

/// The specification revision this module implements.
pub const SPEC_REVISION: &str = "0.4";
/// The date of [`SPEC_REVISION`].
pub const SPEC_DATE: &str = "2026-10-10";

/// `ESC` — starts the OSC and, with `\`, ends it.
const ESC: u8 = 0x1b;
/// `BEL` — the alternative `ST`.
const BEL: u8 = 0x07;

/// Byte prefix of the sequence that starts a program-status OSC.
const PREFIX: &[u8] = b"7501;";
/// Byte body of the shell-integration prompt mark (`OSC 133 ; A`) whose arrival
/// means a new shell prompt begins.
const PROMPT_MARK: &[u8] = b"133;A";

/// Whole sequence limit: `OSC` through `ST`, in bytes. A longer sequence is
/// discarded.
pub const MAX_SEQUENCE: usize = 4096;
/// Key length limit, in bytes.
pub const MAX_KEY: usize = 16;
/// `msg` limit, encoded.
pub const MAX_MSG_ENCODED: usize = 2732;
/// `msg` limit, decoded.
pub const MAX_MSG_DECODED: usize = 2048;
/// `title` limit, encoded.
pub const MAX_TITLE_ENCODED: usize = 256;
/// `title` limit, decoded.
pub const MAX_TITLE_DECODED: usize = 192;
/// `app` limit, in bytes.
pub const MAX_APP: usize = 32;
/// `id` limit: total bytes, bytes per segment, and levels.
pub const MAX_ID: usize = 128;
/// Bytes of one `id` segment.
pub const MAX_ID_SEGMENT: usize = 32;
/// Levels of one `id`.
pub const MAX_ID_LEVELS: usize = 8;
/// Records kept per terminal; the least-recently-updated is evicted above it.
pub const MAX_RECORDS: usize = 256;
/// Records a terminal must at least support.
pub const MIN_RECORDS: usize = 64;

/// The feature-detection reply, and the only bytes this module ever hands back
/// to a program. It contains nothing from any report.
pub const QUERY_REPLY: &[u8] =
    b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\";

/// How often a pane may answer a feature-detection query. Programs may query as
/// fast as they can write; a terminal that answered every one would spend its
/// pty bandwidth on replies.
pub const MIN_REPLY_INTERVAL: Duration = Duration::from_millis(250);

/// What a record says the program is doing. `clear` is not a state: it is a
/// report that removes records (see [`Table::apply`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
}

impl State {
    /// Parses a `state` value; `None` for an unrecognized one, which makes the
    /// whole report ignored.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "idle" => Some(Self::Idle),
            "working" => Some(Self::Working),
            "done" => Some(Self::Done),
            "blocked" => Some(Self::Blocked),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    /// The wire name of the state.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Error => "error",
        }
    }

    /// Whether a record in this state is dropped when the process attached to
    /// the terminal exits or a new shell prompt begins. `done` and `error`
    /// survive both events.
    pub fn dies_with_process(self) -> bool {
        matches!(self, Self::Working | Self::Blocked)
    }

    /// A one-character marker for the chrome. Geometric shapes only: they are
    /// present in every font in the front-end's fallback chain, unlike emoji.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Idle => "○",
            Self::Working => "●",
            Self::Done => "▣",
            Self::Blocked => "▲",
            Self::Error => "✕",
        }
    }

    /// How urgent the state is to a human: higher wins when a pane has several
    /// records and the chrome has room for one.
    pub fn urgency(self) -> u8 {
        match self {
            Self::Blocked => 4,
            Self::Error => 3,
            Self::Working => 2,
            Self::Done => 1,
            Self::Idle => 0,
        }
    }
}

/// Why a `blocked` record is blocked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Permission,
    Question,
    Auth,
}

impl Kind {
    /// Parses a `kind` value; unrecognized values are treated as absent.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "permission" => Some(Self::Permission),
            "question" => Some(Self::Question),
            "auth" => Some(Self::Auth),
            _ => None,
        }
    }

    /// The wire name of the kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
        }
    }
}

/// One record: exactly the keys of the last report that used its id.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    /// The record's id; empty for the root record.
    pub id: String,
    pub state: State,
    pub kind: Option<Kind>,
    pub progress: Option<u8>,
    /// The record's own `app`, if its last report carried one. A program that
    /// wants `app` on a record must repeat it in every report; read
    /// [`Table::app_of`] for the value including inheritance.
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
    /// Logical update time, lowest first out of the table.
    pub updated: u64,
}

impl Record {
    /// Whether `id` is this record or one of its descendants. Records are a
    /// tree by `id` even though a parent record need not exist.
    pub fn covers(&self, id: &str) -> bool {
        self.id == id
            || (id.len() < self.id.len()
                && self.id.starts_with(id)
                && self.id.as_bytes()[id.len()] == b'/')
    }
}

/// What applying a report did, for the tests and for the pane's accounting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    /// A record was created or replaced.
    Updated(String),
    /// Records were removed: the addressed one and its descendants, or every
    /// record when `id` is `None`.
    Cleared { id: Option<String>, removed: usize },
    /// The whole report was discarded: over a limit, undecodable base64, or a
    /// control character in decoded text.
    Rejected,
    /// The report was ignored: no `state`, an unrecognized `state`, or an `id`
    /// that does not match the grammar.
    Ignored,
}

/// The records one terminal (pane) holds.
///
/// Every report replaces its record whole — a key the report omits is missing
/// from the record afterwards — and `clear` removes the addressed record and
/// everything beneath it.
#[derive(Clone, Debug)]
pub struct Table {
    records: Vec<Record>,
    capacity: usize,
    /// Whether `blocked` records are kept when a newcomer needs room. They are:
    /// a blocked record is the one thing the user must act on, and the spec's
    /// own open question is that plain LRU can evict it. Eviction therefore
    /// takes the least-recently-updated *unblocked* record, and falls back to
    /// plain least-recently-updated only when every record is blocked.
    pin_blocked: bool,
    clock: u64,
    /// Records evicted to stay within `capacity`.
    pub evicted: u64,
    /// Reports discarded whole.
    pub rejected: u64,
    /// Reports ignored (no or unknown `state`, bad `id`).
    pub ignored: u64,
}

impl Default for Table {
    fn default() -> Self {
        Self::new()
    }
}

impl Table {
    /// A table with the spec's full capacity of [`MAX_RECORDS`] records.
    pub fn new() -> Self {
        Self::with_capacity(MAX_RECORDS)
    }

    /// A table holding `capacity` records, never below the [`MIN_RECORDS`] the
    /// spec requires a terminal to support.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            records: Vec::new(),
            capacity: capacity.max(MIN_RECORDS),
            pin_blocked: true,
            clock: 0,
            evicted: 0,
            rejected: 0,
            ignored: 0,
        }
    }

    /// The id of the root record.
    pub const ROOT: &'static str = "";

    /// Every record, in insertion order.
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// The number of records held.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the table holds no record.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The record with this exact id, if the table holds one.
    pub fn get(&self, id: &str) -> Option<&Record> {
        self.records.iter().find(|record| record.id == id)
    }

    /// The `app` a record is grouped under: its own if its last report carried
    /// one, otherwise its nearest ancestor's.
    pub fn app_of(&self, id: &str) -> Option<&str> {
        let mut candidate = id;
        loop {
            let record = self.get(candidate)?;
            if let Some(app) = record.app.as_deref() {
                return Some(app);
            }
            match candidate.rsplit_once('/') {
                Some((parent, _)) => candidate = parent,
                None if !candidate.is_empty() => candidate = Self::ROOT,
                None => return None,
            }
        }
    }

    /// The record a pane's chrome should show when it has room for one:
    /// `blocked` outranks `error`, which outranks `working`, then `done`, then
    /// `idle`; ties go to the most recently updated.
    pub fn dominant(&self) -> Option<&Record> {
        self.records
            .iter()
            .max_by_key(|record| (record.state.urgency(), record.updated))
    }

    /// How many records are `blocked` — the ones eviction will not drop.
    pub fn blocked_count(&self) -> usize {
        self.records
            .iter()
            .filter(|record| record.state == State::Blocked)
            .count()
    }

    /// Drops every `working` and `blocked` record, keeping `idle`, `done` and
    /// `error`: what the process attached to the terminal exiting and a new
    /// shell prompt both do. Returns how many records went.
    pub fn drop_transient(&mut self) -> usize {
        let before = self.records.len();
        self.records
            .retain(|record| !record.state.dies_with_process());
        before - self.records.len()
    }

    /// Removes the addressed record and its descendants, or every record when
    /// `id` is `None`. Returns how many went.
    pub fn clear(&mut self, id: Option<&str>) -> usize {
        match id {
            None => {
                let removed = self.records.len();
                self.records.clear();
                removed
            }
            Some(id) => {
                let before = self.records.len();
                let id = id.to_string();
                self.records.retain(|record| !record.covers(&id));
                before - self.records.len()
            }
        }
    }

    /// Applies one report body: the bytes of an `OSC 7501 ; …` sequence between
    /// the `;` and the terminator.
    ///
    /// A malformed pair is skipped and the rest of the report still applies, an
    /// unknown key is ignored, and the last of a repeated key wins. Limits are
    /// checked before any record is touched, so a report that breaks one leaves
    /// the table exactly as it was.
    pub fn apply(&mut self, body: &[u8]) -> Applied {
        let mut pairs = Pairs::default();
        for pair in body.split(|byte| *byte == b':') {
            let Some(eq) = pair.iter().position(|byte| *byte == b'=') else {
                continue;
            };
            let key = trim(&pair[..eq]);
            let value = trim(&pair[eq + 1..]);
            if key.is_empty() || !key.iter().all(|byte| byte.is_ascii_lowercase()) {
                continue;
            }
            // A key over its limit is a limit breach, not a malformed pair.
            if key.len() > MAX_KEY {
                return self.reject();
            }
            if !value.iter().all(|byte| is_value_byte(*byte)) {
                continue;
            }
            match key {
                b"state" => pairs.state = Some(value),
                b"id" => pairs.id = Some(value),
                b"kind" => pairs.kind = Some(value),
                b"progress" => pairs.progress = Some(value),
                b"app" => pairs.app = Some(value),
                b"title" => pairs.title = Some(value),
                b"msg" => pairs.msg = Some(value),
                _ => {}
            }
        }

        let Some(state) = pairs.state.and_then(|value| std::str::from_utf8(value).ok()) else {
            return self.ignore();
        };

        // `id` absent means the root record; for `clear` it means *every*
        // record, so whether an id was given has to survive the parse.
        let (id, explicit_id) = match pairs.id {
            None => (Self::ROOT.to_string(), false),
            Some(value) => {
                let Ok(id) = std::str::from_utf8(value) else {
                    return self.ignore();
                };
                if !valid_id(id) {
                    return self.ignore();
                }
                (id.to_string(), true)
            }
        };

        if state == "clear" {
            let removed = if explicit_id {
                self.clear(Some(&id))
            } else {
                self.clear(None)
            };
            return Applied::Cleared {
                id: explicit_id.then_some(id),
                removed,
            };
        }

        let Some(state) = State::parse(state) else {
            return self.ignore();
        };

        // Free text: checked before any record is touched.
        let title = match decode_free_text(pairs.title, MAX_TITLE_ENCODED, MAX_TITLE_DECODED) {
            Ok(text) => text,
            Err(()) => return self.reject(),
        };
        let msg = match decode_free_text(pairs.msg, MAX_MSG_ENCODED, MAX_MSG_DECODED) {
            Ok(text) => text,
            Err(()) => return self.reject(),
        };

        let kind = if state == State::Blocked {
            pairs
                .kind
                .and_then(|value| std::str::from_utf8(value).ok())
                .and_then(Kind::parse)
        } else {
            None
        };

        let progress = if matches!(state, State::Working | State::Blocked) {
            pairs
                .progress
                .and_then(|value| std::str::from_utf8(value).ok())
                .and_then(parse_progress)
        } else {
            None
        };

        let app = pairs
            .app
            .and_then(|value| std::str::from_utf8(value).ok())
            .filter(|value| valid_app(value))
            .map(str::to_string);

        self.clock += 1;
        let record = Record {
            id: id.clone(),
            state,
            kind,
            progress,
            app,
            title,
            msg,
            updated: self.clock,
        };
        self.store(record);
        Applied::Updated(id)
    }

    fn reject(&mut self) -> Applied {
        self.rejected += 1;
        Applied::Rejected
    }

    fn ignore(&mut self) -> Applied {
        self.ignored += 1;
        Applied::Ignored
    }

    /// Replaces the record with this id, or inserts it, making room first.
    fn store(&mut self, record: Record) {
        if let Some(slot) = self
            .records
            .iter_mut()
            .find(|existing| existing.id == record.id)
        {
            *slot = record;
            return;
        }
        if self.records.len() >= self.capacity {
            self.evict();
        }
        self.records.push(record);
    }

    /// Removes the least-recently-updated record, skipping `blocked` ones while
    /// any unblocked candidate exists.
    fn evict(&mut self) {
        let victim = self
            .records
            .iter()
            .enumerate()
            .filter(|(_, record)| !(self.pin_blocked && record.state == State::Blocked))
            .min_by_key(|(_, record)| record.updated)
            .or_else(|| {
                self.records
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, record)| record.updated)
            })
            .map(|(index, _)| index);
        if let Some(index) = victim {
            self.records.remove(index);
            self.evicted += 1;
        }
    }
}

/// The values of the keys this module knows, borrowed from the report body.
#[derive(Default)]
struct Pairs<'a> {
    state: Option<&'a [u8]>,
    id: Option<&'a [u8]>,
    kind: Option<&'a [u8]>,
    progress: Option<&'a [u8]>,
    app: Option<&'a [u8]>,
    title: Option<&'a [u8]>,
    msg: Option<&'a [u8]>,
}

/// Value character set shared by every key.
fn is_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b',' | b'+' | b'/' | b'=' | b'-')
}

/// Strips the whitespace around a key or value that the spec removes before
/// either is validated.
fn trim(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map(|index| index + 1)
        .unwrap_or(start);
    &bytes[start..end.max(start)]
}

/// Whether `id` matches `segment ("/" segment)*` within the id limits.
pub fn valid_id(id: &str) -> bool {
    if id.is_empty() || id.len() > MAX_ID {
        return false;
    }
    let mut levels = 0;
    for segment in id.split('/') {
        levels += 1;
        if levels > MAX_ID_LEVELS
            || segment.is_empty()
            || segment.len() > MAX_ID_SEGMENT
            || !segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'+' | b'-'))
        {
            return false;
        }
    }
    true
}

/// Whether `app` is a stable program identifier.
pub fn valid_app(app: &str) -> bool {
    !app.is_empty()
        && app.len() <= MAX_APP
        && app
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'+' | b'-'))
}

/// Parses a `progress` value: an integer 0-100, anything else absent.
fn parse_progress(value: &str) -> Option<u8> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse::<u8>().ok().filter(|value| *value <= 100)
}

/// Decodes a base64 free-text value, enforcing the encoded and decoded limits
/// and refusing control characters. `Err` discards the whole report.
fn decode_free_text(
    value: Option<&[u8]>,
    max_encoded: usize,
    max_decoded: usize,
) -> Result<Option<String>, ()> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.len() > max_encoded {
        return Err(());
    }
    let decoded = base64_decode(value).ok_or(())?;
    if decoded.len() > max_decoded {
        return Err(());
    }
    let text = String::from_utf8(decoded).map_err(|_| ())?;
    if text.chars().any(is_control_char) {
        return Err(());
    }
    Ok(Some(text))
}

/// The characters the spec refuses in decoded free text.
fn is_control_char(c: char) -> bool {
    matches!(c, '\u{0000}'..='\u{001f}' | '\u{007f}'..='\u{009f}')
}

/// Standard base64 with optional padding, as the spec's free-text values use.
fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    let mut padding = 0usize;
    for &byte in input {
        if byte == b'=' {
            padding += 1;
            continue;
        }
        if padding > 0 {
            return None;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32;
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // A trailing group of six bits cannot be a byte, and the bits an output
    // byte does not use must be zero (RFC 4648).
    if padding > 2 || bits == 6 || (bits > 0 && acc != 0) {
        return None;
    }
    Some(out)
}

/// Disarms the invisible characters a report may carry for display outside the
/// grid: bidirectional overrides and isolates, zero-width and other formatting
/// characters, the Unicode line and paragraph separators, and anything the
/// spec already refuses. Each is replaced by `U+FFFD` so a reader can see that
/// something was removed instead of being shown text that lies about itself.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| if is_disarmed(c) { '\u{fffd}' } else { c })
        .collect()
}

/// Whether a character must not reach the screen from a record.
///
/// Everything here is either a control character (already refused in decoded
/// text, repeated for safety), a line break, or one of the ranges Unicode
/// describes as causing invisible or reordering effects.
fn is_disarmed(c: char) -> bool {
    is_control_char(c)
        || matches!(c,
            '\u{2028}'
                | '\u{2029}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206f}'
                | '\u{feff}'
                | '\u{fff9}'..='\u{fffb}'
                | '\u{e0000}'..='\u{e007f}')
}

/// One thing the byte scan found in a pty stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The body of an `OSC 7501 ; …` sequence, excluding the `7501;` prefix: a
    /// report, or the single byte `?` for feature detection.
    ProgramStatus(Vec<u8>),
    /// `OSC 133 ; A`: a new shell prompt begins.
    PromptStart,
    /// `ESC c`: the full-reset sequence (RIS).
    FullReset,
}

/// Finds program-status sequences in a pty byte stream, fragment by fragment.
///
/// A sequence split across any read boundaries is found exactly as if it had
/// arrived whole; the buffer never exceeds [`MAX_SEQUENCE`]; and the scan is a
/// pure read — it never consumes or rewrites a byte, so the grid sees the same
/// stream whether or not a sequence was in it.
#[derive(Default)]
pub struct Scanner {
    state: ScanState,
}

#[derive(Default)]
enum ScanState {
    #[default]
    Ground,
    /// Saw `ESC`; the next byte decides.
    Esc,
    /// Inside an OSC body.
    Osc {
        body: Vec<u8>,
        /// Every byte of the OSC body seen, including the ones past the cap.
        seen: usize,
        /// The body outgrew the sequence limit: the sequence is dropped, but
        /// the scan stays in sync by running on to its terminator.
        oversize: bool,
        /// Saw `ESC` inside the body; only `\` makes it a terminator.
        esc: bool,
    },
}

impl Scanner {
    /// A scanner at the start of a stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the scanner is part-way through a sequence, which is what makes
    /// a follow-up chunk continue it rather than start a new one.
    pub fn mid_sequence(&self) -> bool {
        !matches!(self.state, ScanState::Ground)
    }

    /// Feeds one read from the pty, appending what it found to `events`.
    pub fn feed(&mut self, data: &[u8], events: &mut Vec<Event>) {
        for &byte in data {
            match &mut self.state {
                ScanState::Ground => {
                    if byte == ESC {
                        self.state = ScanState::Esc;
                    }
                }
                ScanState::Esc => {
                    self.state = match byte {
                        b']' => ScanState::Osc {
                            body: Vec::new(),
                            seen: 0,
                            oversize: false,
                            esc: false,
                        },
                        // RIS: a full reset. Every other escape is left to the
                        // VT engine, which owns their meaning.
                        b'c' => {
                            events.push(Event::FullReset);
                            ScanState::Ground
                        }
                        _ => ScanState::Ground,
                    };
                }
                ScanState::Osc {
                    body,
                    seen,
                    oversize,
                    esc,
                } => {
                    if *esc {
                        *esc = false;
                        if byte == b'\\' {
                            let body = std::mem::take(body);
                            let oversize = *oversize;
                            self.state = ScanState::Ground;
                            if !oversize {
                                classify(&body, events);
                            }
                            continue;
                        }
                        // An ESC inside a body that is not `ST` abandons the
                        // sequence; the byte itself starts afresh.
                        *body = Vec::new();
                        *seen = 0;
                        *oversize = false;
                        self.state = if byte == ESC {
                            ScanState::Esc
                        } else {
                            ScanState::Ground
                        };
                        continue;
                    }
                    match byte {
                        BEL => {
                            let body = std::mem::take(body);
                            let oversize = *oversize;
                            self.state = ScanState::Ground;
                            if !oversize {
                                classify(&body, events);
                            }
                        }
                        ESC => *esc = true,
                        _ => {
                            *seen += 1;
                            if *seen + 2 + 2 > MAX_SEQUENCE {
                                *oversize = true;
                                body.clear();
                            } else {
                                body.push(byte);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Feeds one read and returns what it found. A convenience for callers that
    /// do not keep an event buffer.
    pub fn scan(&mut self, data: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        self.feed(data, &mut events);
        events
    }
}

/// Turns a finished OSC body into an event, if it is one this module follows.
fn classify(body: &[u8], events: &mut Vec<Event>) {
    if let Some(rest) = body.strip_prefix(PREFIX) {
        events.push(Event::ProgramStatus(rest.to_vec()));
    } else if body == PROMPT_MARK {
        events.push(Event::PromptStart);
    }
}

/// A pane's whole program-status state: the scanner, the records, and the reply
/// allowance.
pub struct Tracker {
    scanner: Scanner,
    table: Table,
    events: Vec<Event>,
    reply_interval: Duration,
    last_reply: Option<Instant>,
    /// Feature-detection replies written back to the program.
    pub replies_sent: u64,
    /// Queries dropped inside the reply interval.
    pub replies_suppressed: u64,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Tracker {
    /// A tracker with the spec's full record capacity.
    pub fn new() -> Self {
        Self::with_options(MAX_RECORDS, MIN_REPLY_INTERVAL)
    }

    /// A tracker with its own record capacity and reply interval, for tests.
    pub fn with_options(capacity: usize, reply_interval: Duration) -> Self {
        Self {
            scanner: Scanner::new(),
            table: Table::with_capacity(capacity),
            events: Vec::new(),
            reply_interval,
            last_reply: None,
            replies_sent: 0,
            replies_suppressed: 0,
        }
    }

    /// The records this pane holds.
    pub fn table(&self) -> &Table {
        &self.table
    }

    /// Feeds pty output and appends any reply owed to the program to `reply`.
    ///
    /// The reply is always [`QUERY_REPLY`] — a constant, so no record's content
    /// can reach a program through this path — and it is written at most once
    /// per `reply_interval`.
    pub fn feed(&mut self, data: &[u8], reply: &mut Vec<u8>) {
        self.events.clear();
        let mut events = std::mem::take(&mut self.events);
        self.scanner.feed(data, &mut events);
        for event in events.drain(..) {
            match event {
                Event::ProgramStatus(body) if body.as_slice() == b"?" => {
                    if self.may_reply() {
                        reply.extend_from_slice(QUERY_REPLY);
                        self.replies_sent += 1;
                        self.last_reply = Some(Instant::now());
                    } else {
                        self.replies_suppressed += 1;
                    }
                }
                Event::ProgramStatus(body) => {
                    self.table.apply(&body);
                }
                Event::PromptStart => {
                    self.table.drop_transient();
                }
                Event::FullReset => {
                    self.table.clear(None);
                }
            }
        }
        self.events = events;
    }

    /// Whether a query may be answered now, which is also what keeps a program
    /// from turning the answer into a way to make the terminal write.
    fn may_reply(&self) -> bool {
        match self.last_reply {
            None => true,
            Some(at) => at.elapsed() >= self.reply_interval,
        }
    }

    /// The child process attached to this pane exited: `working` and `blocked`
    /// records go, `idle`, `done` and `error` stay for the user to find.
    pub fn child_exit(&mut self) {
        self.table.drop_transient();
    }

    /// A new shell prompt began (an `OSC 133 ; A` mark arrived): same lifetime
    /// as the child exiting.
    pub fn prompt_start(&mut self) {
        self.table.drop_transient();
    }

    /// A full reset (RIS): every record goes. A soft reset and a screen switch
    /// do not touch the table, which is why neither has a method here.
    pub fn full_reset(&mut self) {
        self.table.clear(None);
    }
}

/// One record together with the terminal it came from.
pub struct Sourced<'a> {
    /// Display name of the terminal (tab and pane), so a program cannot pretend
    /// to be a program in another terminal.
    pub source: String,
    pub record: &'a Record,
}

/// Flattens live tables into inbox rows: every record, blocked first, then by
/// how recently it was updated.
///
/// A closed pane's records are gone with its table, which is why this takes the
/// live tables rather than a registry of its own.
pub fn inbox<'a>(sources: impl IntoIterator<Item = (String, &'a Table)>) -> Vec<Sourced<'a>> {
    let mut rows: Vec<Sourced<'a>> = Vec::new();
    for (source, table) in sources {
        for record in table.records() {
            rows.push(Sourced {
                source: source.clone(),
                record,
            });
        }
    }
    rows.sort_by(|a, b| {
        b.record
            .state
            .urgency()
            .cmp(&a.record.state.urgency())
            .then(b.record.updated.cmp(&a.record.updated))
            .then(a.source.cmp(&b.source))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec's own example sequence, body only.
    const TERRAFORM: &[u8] = b"state=blocked:kind=permission:app=terraform:msg=QXBwbHkgMyB0byBhZGQsIDEgdG8gY2hhbmdlLCAwIHRvIGRlc3Ryb3k/";

    fn table_with(body: &str) -> Table {
        let mut table = Table::new();
        table.apply(body.as_bytes());
        table
    }

    #[test]
    fn spec_example_report_parses() {
        let table = table_with(std::str::from_utf8(TERRAFORM).unwrap());
        let record = table.get(Table::ROOT).expect("root record");
        assert_eq!(record.state, State::Blocked);
        assert_eq!(record.kind, Some(Kind::Permission));
        assert_eq!(record.app.as_deref(), Some("terraform"));
        assert_eq!(
            record.msg.as_deref(),
            Some("Apply 3 to add, 1 to change, 0 to destroy?")
        );
        assert_eq!(record.title, None);
        assert_eq!(record.progress, None);
    }

    #[test]
    fn unknown_key_is_ignored() {
        let table = table_with("state=working:color=red:app=brew");
        let record = table.get(Table::ROOT).unwrap();
        assert_eq!(record.state, State::Working);
        assert_eq!(record.app.as_deref(), Some("brew"));
    }

    #[test]
    fn duplicate_key_last_wins() {
        let table = table_with("state=idle:state=done:app=brew:app=cargo:msg=QQ==:msg=Qg==");
        let record = table.get(Table::ROOT).unwrap();
        assert_eq!(record.state, State::Done);
        assert_eq!(record.app.as_deref(), Some("cargo"));
        assert_eq!(record.msg.as_deref(), Some("B"));
    }

    #[test]
    fn malformed_pair_is_skipped_and_the_rest_applies() {
        // No `=`, an empty key, a byte outside the value set, an uppercase key.
        let table = table_with("working:state=done:=x:app=br ew:App=x:msg=SGk=");
        let record = table.get(Table::ROOT).unwrap();
        assert_eq!(record.state, State::Done);
        assert_eq!(record.app, None, "value with a space is malformed");
        assert_eq!(record.msg.as_deref(), Some("Hi"));
    }

    #[test]
    fn whitespace_around_keys_and_values_is_removed() {
        let table = table_with("state = working : app = brew ");
        let record = table.get(Table::ROOT).unwrap();
        assert_eq!(record.state, State::Working);
        assert_eq!(record.app.as_deref(), Some("brew"));
    }

    #[test]
    fn missing_state_ignores_the_report() {
        let mut table = Table::new();
        assert_eq!(table.apply(b"app=brew:msg=SGk="), Applied::Ignored);
        assert_eq!(table.len(), 0);
        assert_eq!(table.ignored, 1);
    }

    #[test]
    fn unknown_state_ignores_the_report() {
        let mut table = Table::new();
        assert_eq!(table.apply(b"state=thinking:app=brew"), Applied::Ignored);
        assert_eq!(table.len(), 0, "an unknown state never becomes idle");
    }

    #[test]
    fn invalid_base64_discards_the_whole_report() {
        let mut table = Table::new();
        // A single base64 character cannot carry a byte.
        assert_eq!(table.apply(b"state=working:app=brew:title=A"), Applied::Rejected);
        assert_eq!(table.len(), 0, "nothing from a discarded report applies");
        assert_eq!(table.rejected, 1);
        assert_eq!(table.apply(b"state=working:msg=AAAAA"), Applied::Rejected);
        assert_eq!(table.len(), 0);
        // Non-zero padding bits are not standard base64 either.
        assert_eq!(table.apply(b"state=working:msg=QR"), Applied::Rejected);
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn control_character_in_decoded_text_discards_the_report() {
        let mut table = Table::new();
        // "a\nb" base64-encoded: newline is a control character.
        assert_eq!(
            table.apply(b"state=working:msg=YQpi"),
            Applied::Rejected,
            "a decoded newline discards the report"
        );
        // "a\tb".
        assert_eq!(table.apply(b"state=working:msg=YQli"), Applied::Rejected);
        // DEL and the C1 range.
        assert_eq!(table.apply(b"state=working:msg=YQdi"), Applied::Rejected);
        assert_eq!(table.apply(b"state=working:msg=YQ=="), Applied::Updated(Table::ROOT.into()));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn padded_and_unpadded_base64_both_decode() {
        let table = table_with("state=done:msg=SGVsbG8=");
        assert_eq!(table.get(Table::ROOT).unwrap().msg.as_deref(), Some("Hello"));
        let table = table_with("state=done:msg=SGVsbG8");
        assert_eq!(table.get(Table::ROOT).unwrap().msg.as_deref(), Some("Hello"));
        // Non-ASCII UTF-8 survives the round trip.
        let raw = "π≈3.14159 — ok";
        let encoded = base64_encode(raw.as_bytes());
        let table = table_with(&format!("state=done:msg={encoded}"));
        assert_eq!(table.get(Table::ROOT).unwrap().msg.as_deref(), Some(raw));
    }

    #[test]
    fn msg_and_title_over_their_limits_discard_the_report() {
        let long = "a".repeat(MAX_MSG_DECODED + 1);
        let encoded = base64_encode(long.as_bytes());
        let mut table = Table::new();
        assert_eq!(
            table.apply(format!("state=done:msg={encoded}").as_bytes()),
            Applied::Rejected,
            "one byte past the decoded limit"
        );

        // Exactly at the encoded limit but decoding larger than allowed.
        let at_limit = "a".repeat(MAX_MSG_DECODED);
        let encoded = base64_encode(at_limit.as_bytes());
        assert!(encoded.len() <= MAX_MSG_ENCODED);
        assert_eq!(
            table.apply(format!("state=done:msg={encoded}").as_bytes()),
            Applied::Updated(Table::ROOT.into()),
            "a msg at both limits is accepted"
        );

        let title = base64_encode("t".repeat(MAX_TITLE_DECODED + 1).as_bytes());
        assert_eq!(
            table.apply(format!("state=done:title={title}").as_bytes()),
            Applied::Rejected
        );
        let title = base64_encode("t".repeat(MAX_TITLE_DECODED).as_bytes());
        assert_eq!(title.len(), MAX_TITLE_ENCODED);
        assert_eq!(
            table.apply(format!("state=done:title={title}").as_bytes()),
            Applied::Updated(Table::ROOT.into())
        );
    }

    #[test]
    fn over_long_key_discards_the_whole_report() {
        let mut table = Table::new();
        let key = "k".repeat(MAX_KEY + 1);
        assert_eq!(
            table.apply(format!("state=working:{key}=x").as_bytes()),
            Applied::Rejected
        );
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn id_past_eight_levels_or_128_bytes_is_ignored() {
        let mut table = Table::new();
        table.apply(b"state=working:app=deploy");

        let deep = vec!["a"; MAX_ID_LEVELS + 1].join("/");
        assert_eq!(
            table.apply(format!("state=done:id={deep}").as_bytes()),
            Applied::Ignored,
            "nine levels"
        );
        let long_segment = "b".repeat(MAX_ID_SEGMENT + 1);
        assert_eq!(
            table.apply(format!("state=done:id={long_segment}").as_bytes()),
            Applied::Ignored
        );
        let long_id = vec!["c"; 6].join("/"); // 11 bytes; pad with segments to pass 128
        let padded = format!("{long_id}/{}", "d".repeat(MAX_ID_SEGMENT));
        assert!(padded.len() < MAX_ID);
        assert_eq!(
            table.apply(format!("state=done:id={padded}").as_bytes()),
            Applied::Updated(padded),
            "a legal, deep id is accepted"
        );
        // A record past the byte limit is ignored, and the root is untouched.
        let bytes = vec!["e".repeat(MAX_ID_SEGMENT); MAX_ID_LEVELS].join("/");
        assert!(bytes.len() > MAX_ID);
        assert_eq!(
            table.apply(format!("state=done:id={bytes}").as_bytes()),
            Applied::Ignored
        );
        assert_eq!(
            table.get(Table::ROOT).unwrap().state,
            State::Working,
            "a malformed id never falls back to the root record"
        );
    }

    #[test]
    fn empty_id_value_is_ignored() {
        let mut table = Table::new();
        assert_eq!(table.apply(b"state=done:id="), Applied::Ignored);
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn progress_is_only_kept_for_working_and_blocked() {
        let table = table_with("state=working:progress=40");
        assert_eq!(table.get(Table::ROOT).unwrap().progress, Some(40));
        let table = table_with("state=blocked:progress=0");
        assert_eq!(table.get(Table::ROOT).unwrap().progress, Some(0));
        let table = table_with("state=done:progress=40");
        assert_eq!(table.get(Table::ROOT).unwrap().progress, None);
        let table = table_with("state=idle:progress=100");
        assert_eq!(table.get(Table::ROOT).unwrap().progress, None);
    }

    #[test]
    fn non_numeric_or_out_of_range_progress_is_absent() {
        for value in ["abc", "4.5", "-1", "101", "", "40%"] {
            let table = table_with(&format!("state=working:progress={value}"));
            assert_eq!(
                table.get(Table::ROOT).unwrap().progress,
                None,
                "progress={value:?} is absent"
            );
        }
        let table = table_with("state=working:progress=100");
        assert_eq!(table.get(Table::ROOT).unwrap().progress, Some(100));
        let table = table_with("state=working:progress=00");
        assert_eq!(table.get(Table::ROOT).unwrap().progress, Some(0));
        // Whitespace around the value is removed before it is read.
        let table = table_with("state=working:progress=  5  ");
        assert_eq!(table.get(Table::ROOT).unwrap().progress, Some(5));
    }

    #[test]
    fn kind_is_only_meaningful_with_blocked() {
        for kind in ["permission", "question", "auth"] {
            let table = table_with(&format!("state=blocked:kind={kind}"));
            assert_eq!(
                table.get(Table::ROOT).unwrap().kind,
                Kind::parse(kind),
                "{kind} with blocked"
            );
            let table = table_with(&format!("state=working:kind={kind}"));
            assert_eq!(table.get(Table::ROOT).unwrap().kind, None, "{kind} ignored");
        }
        let table = table_with("state=blocked:kind=approval");
        assert_eq!(table.get(Table::ROOT).unwrap().kind, None, "unrecognized kind");
    }

    #[test]
    fn app_outside_its_charset_is_absent_not_a_discard() {
        let table = table_with("state=working:app=/usr/bin/cargo");
        let record = table.get(Table::ROOT).unwrap();
        assert_eq!(record.app, None);
        assert_eq!(record.state, State::Working, "the report still applies");
        let table = table_with(&format!("state=working:app={}", "a".repeat(MAX_APP + 1)));
        assert_eq!(table.get(Table::ROOT).unwrap().app, None);
        let table = table_with("state=working:app=claude-code");
        assert_eq!(table.get(Table::ROOT).unwrap().app.as_deref(), Some("claude-code"));
    }

    #[test]
    fn replacement_is_whole() {
        let mut table = Table::new();
        table.apply(b"state=blocked:kind=permission:app=terraform:msg=QQ==:title=Qg==");
        let record = table.get(Table::ROOT).unwrap();
        assert_eq!(record.kind, Some(Kind::Permission));
        assert_eq!(record.title.as_deref(), Some("B"));

        table.apply(b"state=working:app=terraform");
        let record = table.get(Table::ROOT).unwrap();
        assert_eq!(record.state, State::Working);
        assert_eq!(record.kind, None, "a key the report omits is gone");
        assert_eq!(record.msg, None);
        assert_eq!(record.title, None);
        assert_eq!(record.progress, None);
        assert_eq!(record.app.as_deref(), Some("terraform"), "repeated app survives");
    }

    #[test]
    fn clear_removes_the_record_and_its_descendants_only() {
        let mut table = Table::new();
        table.apply(b"state=working:app=deploy");
        table.apply(b"state=working:id=us-east:title=VVMgRWFzdA==:progress=40");
        table.apply(b"state=blocked:kind=permission:id=eu-west");
        table.apply(b"state=done:id=us-east/sub");
        table.apply(b"state=idle:id=other");

        let applied = table.apply(b"state=clear:id=us-east");
        assert_eq!(
            applied,
            Applied::Cleared {
                id: Some("us-east".into()),
                removed: 2
            }
        );
        assert!(table.get("us-east").is_none());
        assert!(table.get("us-east/sub").is_none());
        assert!(table.get("eu-west").is_some());
        assert!(table.get("other").is_some());
        assert_eq!(table.get(Table::ROOT).unwrap().state, State::Working);
    }

    #[test]
    fn clear_without_an_id_removes_every_record() {
        let mut table = Table::new();
        table.apply(b"state=working");
        table.apply(b"state=blocked:id=eu-west");
        assert_eq!(
            table.apply(b"state=clear"),
            Applied::Cleared {
                id: None,
                removed: 2
            }
        );
        assert!(table.is_empty());
    }

    #[test]
    fn app_is_inherited_from_the_nearest_ancestor() {
        let mut table = Table::new();
        table.apply(b"state=working:app=deploy");
        table.apply(b"state=working:id=us-east:title=VVMgRWFzdA==:progress=40");
        table.apply(b"state=working:id=us-east/sub");
        table.apply(b"state=working:id=other");
        table.apply(b"state=done:app=backup:id=other/child");

        assert_eq!(table.app_of(Table::ROOT), Some("deploy"));
        assert_eq!(table.app_of("us-east"), Some("deploy"));
        assert_eq!(table.app_of("us-east/sub"), Some("deploy"));
        assert_eq!(table.app_of("other"), Some("deploy"));
        assert_eq!(table.app_of("other/child"), Some("backup"), "nearest ancestor wins");
        assert_eq!(table.app_of("nothing/here"), None, "no record, no app");
    }

    #[test]
    fn a_parent_record_need_not_exist() {
        let mut table = Table::new();
        table.apply(b"state=working:app=deploy:id=build/test");
        assert_eq!(table.len(), 1);
        assert_eq!(table.app_of("build/test"), Some("deploy"));
        assert!(table.get("build").is_none());
    }

    #[test]
    fn records_at_capacity_evict_the_least_recently_updated() {
        let mut table = Table::with_capacity(MAX_RECORDS);
        assert_eq!(table.len(), 0);
        for i in 0..MAX_RECORDS {
            assert_eq!(
                table.apply(format!("state=working:id=w/{i}").as_bytes()),
                Applied::Updated(format!("w/{i}"))
            );
        }
        assert_eq!(table.len(), MAX_RECORDS);
        assert_eq!(table.evicted, 0);

        table.apply(b"state=working:id=w/new");
        assert_eq!(table.len(), MAX_RECORDS, "capacity holds");
        assert_eq!(table.evicted, 1);
        assert!(table.get("w/0").is_none(), "the oldest went");
        assert!(table.get("w/1").is_some());
        assert!(table.get("w/new").is_some());

        // Updating a record makes it the most recently used.
        table.apply(b"state=idle:id=w/1");
        table.apply(b"state=working:id=w/another");
        assert!(table.get("w/1").is_some(), "recently updated survives");
        assert!(table.get("w/2").is_none(), "the next oldest went");
    }

    #[test]
    fn eviction_does_not_silently_drop_a_blocked_record() {
        let mut table = Table::with_capacity(MIN_RECORDS);
        table.apply(b"state=blocked:kind=permission:app=terraform:id=w/0");
        for i in 1..MIN_RECORDS {
            table.apply(format!("state=working:id=w/{i}").as_bytes());
        }
        assert_eq!(table.len(), MIN_RECORDS);

        table.apply(b"state=working:id=w/new");
        assert_eq!(table.evicted, 1);
        assert!(
            table.get("w/0").is_some(),
            "the oldest record is blocked, so it is pinned and w/1 goes"
        );
        assert!(table.get("w/1").is_none());
        assert_eq!(table.blocked_count(), 1);

        // With every record blocked, eviction falls back to plain LRU: the
        // table cannot grow without bound to keep them all.
        let mut all_blocked = Table::with_capacity(MIN_RECORDS);
        for i in 0..MIN_RECORDS {
            all_blocked.apply(format!("state=blocked:id=b/{i}").as_bytes());
        }
        all_blocked.apply(b"state=blocked:id=b/new");
        assert_eq!(all_blocked.len(), MIN_RECORDS);
        assert_eq!(all_blocked.blocked_count(), MIN_RECORDS);
        assert!(all_blocked.get("b/0").is_none(), "still bounded");
    }

    #[test]
    fn dominant_record_prefers_blocked_then_error_then_working() {
        let mut table = Table::new();
        table.apply(b"state=working:app=deploy");
        assert_eq!(table.dominant().unwrap().state, State::Working);
        table.apply(b"state=done:id=us-east");
        assert_eq!(table.dominant().unwrap().state, State::Working);
        table.apply(b"state=error:id=eu-west");
        assert_eq!(table.dominant().unwrap().state, State::Error);
        table.apply(b"state=blocked:kind=auth:id=ap-south");
        assert_eq!(table.dominant().unwrap().state, State::Blocked);
    }

    #[test]
    fn child_exit_drops_working_and_blocked_only() {
        let mut table = Table::new();
        table.apply(b"state=working:id=w");
        table.apply(b"state=blocked:id=b");
        table.apply(b"state=idle:id=i");
        table.apply(b"state=done:id=d");
        table.apply(b"state=error:id=e");
        assert_eq!(table.drop_transient(), 2);
        assert!(table.get("w").is_none());
        assert!(table.get("b").is_none());
        assert!(table.get("i").is_some());
        assert!(table.get("d").is_some());
        assert!(table.get("e").is_some());
    }

    #[test]
    fn sanitize_disarms_bidi_and_invisible_formatting() {
        let attack = "safe\u{202e}gnp.exe\u{200b}\u{2066}\u{feff}\u{2028}end";
        let cleaned = sanitize(attack);
        assert!(!cleaned.contains('\u{202e}'), "right-to-left override gone");
        assert!(!cleaned.contains('\u{2066}'), "isolate gone");
        assert!(!cleaned.contains('\u{200b}'), "zero width space gone");
        assert!(!cleaned.contains('\u{feff}'), "bom gone");
        assert!(!cleaned.contains('\u{2028}'), "line separator gone");
        assert!(cleaned.contains("safe"));
        assert!(cleaned.contains("gnp.exe"));
        assert!(cleaned.contains("end"));
        assert_eq!(cleaned.matches('\u{fffd}').count(), 5);
        // Ordinary text, including a normal space and punctuation, is untouched.
        assert_eq!(sanitize("Upgraded 12 packages — 100%"), "Upgraded 12 packages — 100%");
    }

    #[test]
    fn scanner_finds_a_whole_sequence() {
        let mut scanner = Scanner::new();
        let events = scanner.scan(b"\x1b]7501;state=working:app=brew\x1b\\");
        assert_eq!(
            events,
            vec![Event::ProgramStatus(b"state=working:app=brew".to_vec())]
        );
        assert!(!scanner.mid_sequence());
    }

    #[test]
    fn scanner_accepts_bel_as_a_terminator() {
        let mut scanner = Scanner::new();
        let events = scanner.scan(b"\x1b]7501;state=done:msg=SGk=\x07");
        assert_eq!(
            events,
            vec![Event::ProgramStatus(b"state=done:msg=SGk=".to_vec())]
        );
    }

    #[test]
    fn scanner_is_fragment_tolerant_at_every_split() {
        let bytes: &[u8] = b"\x1b]7501;state=blocked:kind=permission:app=terraform\x1b\\";
        // Split after every byte, including inside the prefix and the terminator.
        for split in 1..bytes.len() {
            let mut scanner = Scanner::new();
            let mut events = Vec::new();
            scanner.feed(&bytes[..split], &mut events);
            scanner.feed(&bytes[split..], &mut events);
            assert_eq!(
                events,
                vec![Event::ProgramStatus(
                    b"state=blocked:kind=permission:app=terraform".to_vec()
                )],
                "split at {split}"
            );
        }
        // And byte by byte.
        let mut scanner = Scanner::new();
        let mut events = Vec::new();
        for byte in bytes {
            scanner.feed(std::slice::from_ref(byte), &mut events);
        }
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn scanner_leaves_other_sequences_alone() {
        let mut scanner = Scanner::new();
        let stream = b"\x1b]0;title\x07plain\x1b]11;?\x1b\\\x1b[31mred\x1b\\";
        let events = scanner.scan(stream);
        assert!(events.is_empty(), "no program-status sequence here");
        assert!(!scanner.mid_sequence());
    }

    #[test]
    fn scanner_reports_prompt_marks_and_full_resets() {
        let mut scanner = Scanner::new();
        let events = scanner.scan(b"\x1b]133;A\x07\x1b]7501;state=working\x1b\\\x1bc");
        assert_eq!(
            events,
            vec![
                Event::PromptStart,
                Event::ProgramStatus(b"state=working".to_vec()),
                Event::FullReset,
            ]
        );
    }

    #[test]
    fn scanner_drops_an_over_long_sequence_and_stays_in_sync() {
        let mut scanner = Scanner::new();
        let mut stream = Vec::new();
        stream.extend_from_slice(b"\x1b]7501;state=working:msg=");
        stream.extend(std::iter::repeat(b'A').take(MAX_SEQUENCE));
        stream.extend_from_slice(b"\x1b\\");
        // What follows the over-long sequence is still found.
        stream.extend_from_slice(b"\x1b]7501;state=done\x1b\\");
        let events = scanner.scan(&stream);
        assert_eq!(
            events,
            vec![Event::ProgramStatus(b"state=done".to_vec())],
            "the over-long sequence is dropped, the next one is not"
        );
        assert!(!scanner.mid_sequence());
    }

    #[test]
    fn scanner_recovers_from_an_escape_inside_a_body() {
        let mut scanner = Scanner::new();
        let events = scanner.scan(b"\x1b]7501;state=wo\x1b[31mrk\x1b\\\x1b]7501;state=done\x1b\\");
        assert_eq!(
            events,
            vec![Event::ProgramStatus(b"state=done".to_vec())],
            "the abandoned body is not a report, the scan keeps going"
        );
    }

    #[test]
    fn a_sequence_split_across_fragments_applies_once() {
        let mut tracker = Tracker::new();
        let mut reply = Vec::new();
        for chunk in [
            &b"\x1b]75"[..],
            b"01;state=blocked:kind=auth:app=",
            b"brew:msg=UGFzc3dvcmQgcmVxdWlyZWQ=",
            b"\x1b",
            b"\\",
        ] {
            tracker.feed(chunk, &mut reply);
        }
        let record = tracker.table().get(Table::ROOT).unwrap();
        assert_eq!(record.state, State::Blocked);
        assert_eq!(record.kind, Some(Kind::Auth));
        assert_eq!(record.app.as_deref(), Some("brew"));
        assert_eq!(record.msg.as_deref(), Some("Password required"));
        assert!(reply.is_empty(), "a report gets no answer");
    }

    #[test]
    fn query_is_answered_with_the_constant_and_never_with_report_content() {
        let mut tracker = Tracker::new();
        let mut reply = Vec::new();
        tracker.feed(
            b"\x1b]7501;state=blocked:app=terraform:msg=U0VDUkVULU1BUktFUi00Mg==\x1b\\\x1b]7501;?\x1b\\",
            &mut reply,
        );
        assert_eq!(reply, QUERY_REPLY);
        let text = String::from_utf8(reply.clone()).unwrap();
        assert!(!text.contains("SECRET"), "no record content is echoed");
        assert!(!text.contains("terraform"));
        assert_eq!(tracker.replies_sent, 1);
        // A query split across fragments is answered once.
        let mut tracker = Tracker::new();
        let mut reply = Vec::new();
        tracker.feed(b"\x1b]7501;", &mut reply);
        tracker.feed(b"?\x1b\\", &mut reply);
        assert_eq!(reply, QUERY_REPLY);
        assert_eq!(tracker.replies_sent, 1);
    }

    #[test]
    fn queries_are_rate_limited() {
        let mut tracker = Tracker::with_options(MAX_RECORDS, Duration::from_millis(60));
        let mut reply = Vec::new();
        for _ in 0..5 {
            tracker.feed(b"\x1b]7501;?\x1b\\", &mut reply);
        }
        assert_eq!(tracker.replies_sent, 1, "one answer inside the window");
        assert_eq!(tracker.replies_suppressed, 4);
        std::thread::sleep(Duration::from_millis(80));
        tracker.feed(b"\x1b]7501;?\x1b\\", &mut reply);
        assert_eq!(tracker.replies_sent, 2, "answered again after the window");
        assert_eq!(reply.len(), QUERY_REPLY.len() * 2);
    }

    #[test]
    fn lifetime_events_drive_the_table_through_the_tracker() {
        let mut tracker = Tracker::new();
        let mut reply = Vec::new();
        tracker.feed(b"\x1b]7501;state=working:id=w\x1b\\\x1b]7501;state=done:id=d\x1b\\", &mut reply);
        assert_eq!(tracker.table().len(), 2);
        tracker.feed(b"\x1b]133;A\x1b\\", &mut reply);
        assert!(tracker.table().get("w").is_none(), "prompt drops working");
        assert!(tracker.table().get("d").is_some(), "done survives the prompt");
        tracker.feed(b"\x1b]7501;state=blocked:id=b\x1b\\", &mut reply);
        tracker.child_exit();
        assert!(tracker.table().get("b").is_none(), "child exit drops blocked");
        tracker.feed(b"\x1b]7501;state=done:id=d2\x1b\\", &mut reply);
        tracker.feed(b"\x1bc", &mut reply);
        assert!(tracker.table().is_empty(), "RIS clears everything");
    }

    #[test]
    fn inbox_sorts_blocked_first_and_carries_its_source() {
        let mut idle = Table::new();
        idle.apply(b"state=idle:app=zsh");
        let mut blocked = Table::new();
        blocked.apply(b"state=blocked:kind=permission:app=terraform:msg=QQ==");
        let rows = inbox(vec![
            ("Tab 1 / pane 0".to_string(), &idle),
            ("Tab 2 / pane 1".to_string(), &blocked),
        ]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].record.state, State::Blocked);
        assert_eq!(rows[0].source, "Tab 2 / pane 1");
        assert_eq!(rows[1].record.state, State::Idle);
        assert_eq!(rows[1].source, "Tab 1 / pane 0");
    }

    #[test]
    fn records_go_away_with_their_table() {
        let mut table = Table::new();
        table.apply(b"state=blocked:id=b");
        {
            let tables = vec![("gone".to_string(), &table)];
            assert_eq!(inbox(tables).len(), 1);
        }
        // A closed pane drops its table: nothing left to list.
        drop(table);
        let empty: Vec<(String, &Table)> = Vec::new();
        assert!(inbox(empty).is_empty());
    }

    /// Base64 for the tests' own text; the decoder is what is under test, and
    /// this encoder is checked against literals above.
    fn base64_encode(input: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in input.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(ALPHABET[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }
}
