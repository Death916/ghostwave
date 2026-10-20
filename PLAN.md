# PLAN — answer a font query so a program can learn ghostwave's real font

Card: `t_c989ef4e` (REDO, stage 1 PLAN). Repo: `ghostwave`, base `master` = `1a08166`.
Branch: `wt/t_c989ef4e`, worktree `.worktrees/t_c989ef4e`. **Nothing is pushed; hermz publishes.**

`relay: job=t_c989ef4e`

---

## 0. PREMISE CHECK — read this first (measured, not assumed)

The card says the missing thing is "the font query (**OSC 12**)" and that answering it makes
`fastfetch` show ghostwave's real font. Both halves are wrong, and stage 3 cannot build a
correct reply on top of them. Evidence:

**a) OSC 12 is the cursor colour, not a font.** xterm/ECMA-48 assign 10–19 to "dynamic
colours"; 12 = cursor colour (`OSC 12 ; rgb:RRRR/GGGG/BBBB`), and the query form is
`OSC 12 ; ?`. Sources: <https://ghostty.org/docs/vt/osc/1x> (table: `12` → "Cursor color"),
<https://terminalguide.namepad.de/seq/osc-12/> ("Change/Read Cursor Color"), Alacritty's
escape table (`OSC 12 | IMPLEMENTED`). No terminal defines a font reply for OSC 12, so a
reply would have to be invented, and libghostty-vt would also answer the same query with a
colour (it owns 4/10–19 — see §1), i.e. the program would receive two replies to one query.

**b) At this pin the VT cannot even tell us "that was a font query".** `libghostty-vt` 0.2.1's
Rust surface (`~/.cargo/registry/src/…/libghostty-vt-0.2.1/src/osc.rs:170-198`) has
`CommandType` = title/icon/prompt/clipboard/pwd/mouse-shape/colour/kitty-colour/notification/
hyperlink/conemu/text-sizing — no font command — and the C enum it mirrors
(`libghostty-vt-sys-0.2.1/src/bindings.rs:3101-3127`, `OscCommandType`) has the same 22 values.
The effect callbacks are the same list as `src/term.rs:202-269`: `on_pty_write`, `on_bell`,
`on_enquiry`, `on_xtversion`, `on_title_changed`, `on_pwd_changed`, `on_size`,
`on_color_scheme`, `on_device_attributes`, `on_clipboard_write` — nothing font-shaped, and no
DCS/XTGETTCAP surface at all (`grep -rn "gettcap\|dcs" src/` in that crate: empty).

**c) `fastfetch` never sends an OSC font query on Linux.** In `fastfetch` (installed here:
2.63.1, source `src/detection/terminalfont/terminalfont.c` @ `dev`) every Linux path reads
config files or runs a terminal-specific helper; the only escape-sequence font query in the
whole module is in `detectKitty()`:

```
"\eP+q6b697474792d71756572792d666f6e745f66616d696c79;6b697474792d71756572792d666f6e745f73697a65\e\\"
```

i.e. **XTGETTCAP** (`DCS +q <hex-cap>`) for the caps `kitty-query-font_family` /
`kitty-query-font_size`, expecting `DCS 1+r <hexkey>=<hexvalue> ST` — and that branch only
runs when the terminal's process name is `kitty` (`terminalfont.c:421`), which ghostwave can
never be. `ghostwave` matches nothing in the common or Linux matcher, so the module errors.
Measured on orac right now under a non-matching parent:

```
$ fastfetch -s font:terminalfont --format json
[ { "type": "Font", "error": "No fonts found" },
  { "type": "TerminalFont", "error": "Unknown terminal: hermes" } ]
```

So ghostwave's symptom is `Unknown terminal: ghostwave` (fastfetch's `terminalfont` module), or
a *system* font from fastfetch's separate `font` module / a `-c examples/N.jsonc` preset — the
owner runs `fastfetch -c examples/$preset.jsonc` (`~/nixconfig/home-manager/home.nix:306-312`).
The "Noto Sans" the card reports is that system-font path (fontconfig default), and **no escape
sequence ghostwave can emit changes it.** Fixing the fastfetch *output* needs a fastfetch change
(add `ghostwave` to its matcher) — upstream, out of scope; noted in §6.

**d) The only documented *font* query a terminal answers is OSC 50** (`ESC ] 50 ; ? ST`), xterm's
"Change/Query Font": with no characters after `?` the terminal replies with the current font as
`ESC ] 50 ; <font> ESC \` (<https://terminalguide.namepad.de/seq/osc-50_xterm/>). libghostty-vt
parses no OSC 50 at all (same enum evidence as (b)), so there is no double reply and nothing to
override.

### Decision for stage 2 (plan review) — pick one, this is what stage 3 builds

* **RECOMMENDED — Option A: answer the xterm font query, `OSC 50 ; ?`.** It is the real
  font query, its reply is a plain font name (assertable character-for-character), it needs no
  invented format, and it collides with nothing in the VT. Satisfies the card's acceptance
  ("feeds the font query, asserts the configured font name comes back"). It does **not** change
  what `fastfetch -c examples/N.jsonc` prints — nothing in ghostwave can (see (c)).
* **Option B: also/instead answer XTGETTCAP `DCS +q` for the two kitty caps.** Answering the
  exact bytes fastfetch sends, in fastfetch's hex reply shape. Rejected as scope: the only
  program known to send it gates on `processName == "kitty"`, so it reaches zero extra
  consumers; a second protocol doubles the encoding surface for nothing.
* **Option C: answer OSC 12 with a font (the card as literally written).** Rejected: §0(a) —
  it redefines a colour query, would need ghostwave to strip the query out of the byte stream
  to avoid libghostty-vt's own colour reply, and breaks cursor-colour queries for every other
  program.

If the ruling is B or C, stage 3 is a different job; the rest of this plan (hook point, plumbing,
test shape) holds for any of them, only the scanner's grammar and reply encoder change.

---

## 1. What ghostwave does with an inbound OSC 12 today (traced)

1. `src/pty.rs` reader thread → `PtyMsg::Data` on the pane's channel.
2. **Live path:** `src/main.rs:253-259` (`Message::Output`) → `Pane::ingest(&data)`
   (`src/pane.rs:332-343`): appends to the pane log, then
   `self.term.term.vt_write(data)` (`pane.rs:340`). No scan, no interception — the bytes go to
   the VT verbatim.
3. **Restore/prime path:** `TermPane::poll_pty` (`src/term.rs:417-456`) → `self.term.vt_write(&data)`
   (`term.rs:439`), drained synchronously by `prime_pty` (`term.rs:349-371`).
4. Inside libghostty-vt, `OSC 1 2 ; ?` is `osc.color_operation` (10–19) — Ghostty's own docs put
   `n=12` at cursor colour. libghostty-vt answers colour queries itself through the
   `on_pty_write` callback installed at `term.rs:202-210` (`write_raw(fd, data)`), which is why
   the crate's own test at `term.rs:1121-1124` sees an OSC 11 reply without any app code.
   **Nothing in ghostwave turns this into a font answer, and nothing currently answers any font
   query at all.**
5. Replay is separate and must stay separate: `session::replay_pane_log` (`src/session.rs:378-392`)
   feeds recorded history to `vt_write`. Recorded queries are historical; answering them would
   inject replies into a brand-new shell.

## 2. Where the real font setting lives (do not hardcode)

`font_family` (optional) + `font_size` in `~/.config/ghostwave/config.toml`
(`src/config.rs:10-12`, `Config::path()` `config.rs:191-200`, `Config::load()` `config.rs:177-189`)
→ `main.rs:738` `ui_fonts::init(config.font_family.as_deref())` → `resolve_family()` matches it
against `fc-list` and stores the **resolved** family in `ui_fonts::ACTIVE_FAMILY`
(`src/ui_fonts.rs:14,19-36`); unset/unfound falls back to the bundled face, family
`"JetBrains Mono"` (`ui_fonts.rs:34,40`). `ui_fonts::primary()` (`ui_fonts.rs:39-56`) reads the
same `ACTIVE_FAMILY`, so it is the single source of truth. `init` runs before any pane is created.

## 3. Design (Option A) — the smallest honest scope

Everything happens **above** the VT, on the same inbound bytes, in the pane. `libghostty-vt` is
not touched (pinned 0.2.1), and the bytes the VT receives are unchanged: the scanner only reads
the slice.

1. **`src/ui_fonts.rs`** — one new accessor:
   `pub fn active_family() -> &'static str` → `ACTIVE_FAMILY.get()` or `"JetBrains Mono"`.
   Doc comment states: the family the cells are measured from, i.e. what a font query must
   answer.
2. **`src/font_query.rs`** (new, std only — same shape as `src/program_status.rs` on the
   `wt/osc7501` branch, which is the house pattern for scanning pty bytes):
   `pub struct Scanner { buf: [u8; N], state }` with
   `pub fn feed(&mut self, data: &[u8], family: &str) -> Vec<u8>` returning zero or more complete
   replies. Grammar, exactly:
   * start: `ESC ]` (`0x1b 0x5d`); body accumulates until `BEL` (`0x07`) or `ESC \` (`0x1b 0x5c`).
   * retain at most 64 body bytes (`"50;?"` is 4); a longer body abandons the sequence and
     returns to idle, so an OSC 8 hyperlink or a clipboard payload is never buffered.
   * a retained body is a query **only** when it is exactly `50;?` — `50;` + anything else is a
     *set* command and is ignored; `12;?`, `4;0;?`, `52;…` etc. are ignored.
   * reply: `ESC ] 50 ; <family> ESC \`, echoing the terminator the query used (BEL → `BEL`),
     matching the VT's own convention ("try to match the terminator used in the request").
   * fragment tolerance: state survives across `feed` calls, so a query split by the pty reader
     still gets exactly one reply.
3. **`src/term.rs`** — `TermPane` gains `font_query: crate::font_query::Scanner` and
   `pub font_family: String` (captured once in `TermPane::new` from `ui_fonts::active_family()`,
   so every pane answers what the app is actually configured to use). One new module-private
   helper `fn scan_queries(&mut self, data: &[u8])` that appends `scanner.feed(data, &self.font_family)`
   to the existing `self.response` buffer (`term.rs:91`, flushed by `flush_response`,
   `term.rs:517-524`). Call it at the top of the `PtyMsg::Data` arm in `poll_pty`
   (`term.rs:434-441`), before `vt_write`.
4. **`src/pane.rs`** — `Pane::ingest` (`pane.rs:332-343`) calls `self.term.scan_queries(data)`
   before `vt_write` and then `self.term.flush_response()` (the iced path never flushed responses
   before; `flush_response` is a no-op when the buffer is empty, and `write_input` already writes
   to the pty from this same thread).
5. **Not touched:** `session::replay_pane_log` (no answering of replayed bytes), `src/input.rs`
   (keyboard only — it has no part in inbound OSC), the VT crate, `main.rs` beyond nothing.

## 4. Tests (assertable without a real terminal)

`src/font_query.rs` `#[cfg(test)]`, driving the scanner over raw bytes:

| test | input | assert |
|---|---|---|
| `answers_the_font_query` | `b"\x1b]50;?\x1b\\"`, family `"Fira Code"` | reply == `b"\x1b]50;Fira Code\x1b\\"` |
| `echoes_bel_terminator` | `b"\x1b]50;?\x07"` | reply ends `\x07`, not `ESC \` |
| `split_across_feeds_answers_once` | `b"\x1b]50"` then `b";?\x1b\\"` | one reply, same bytes as above |
| `ignores_set_command` | `b"\x1b]50;Monospace\x1b\\"` | empty |
| `ignores_other_oscs` | `b"\x1b]12;?\x1b\\"`, `b"\x1b]4;0;?\x1b\\"` | empty |
| `oversized_body_is_abandoned` | `ESC]50;` + 200 × `x` + `ST` | empty, and the next valid query still answers |

Plus one pane-level test in `src/term.rs`'s test module (the card's acceptance line): build a pane
without a child — `TermPane::new(80, 24, cell, None, &[], None, 1000, 1<<20, false, None)` — feed
`b"\x1b]50;?\x1b\\"` through the same entry point the live path uses, and assert the bytes in
`pane.response` decode to `ESC ] 50 ; <ui_fonts::active_family()> ESC \`. The expected family is
read from `ui_fonts::active_family()` at assert time, so the test is order-independent w.r.t. any
other test calling `ui_fonts::init`.

No test needs a pty, a display, or a child process; the reply is asserted at the term layer,
exactly like `terminal_responds_to_probes` (`term.rs:1069-1132`).

## 5. Verification commands (stage 3 runs these and pastes the output)

```
export CARGO_TARGET_DIR=/var/build/ghostwave/t_c989ef4e
cd .worktrees/t_c989ef4e
bash ~/.hermes/scripts/build_gate.sh nix-shell --run 'cargo test'
```

* Expected: green, `216` tests on `master` **plus** the 7 new ones (`cargo test font_query`
  lists them). Exit 3 from the gate = lock held → comment + block `transient`, never build anyway;
  exit 4 = low RAM → retry.
* `bash ~/.hermes/scripts/build_gate.sh nix-shell --run 'cargo test font_query -- --nocapture'`
  to show the new test names on their own.
* Manual end-to-end (needs the GUI, so it is a `NEEDS-VULCAN:` line, not a stage-3 acceptance):
  in a ghostwave pane run
  `printf '\033]50;?\033\\'; timeout 1 cat -v` — expect `^[]50;JetBrains Mono^[\` (or the
  configured family).

Baseline for comparison is the card's `216`; if stage 3 measures something else on `master`, that
number wins and the card records the difference.

## 6. Risks / traps

* **Double replies.** Only a risk for OSC 12 (Option C), not OSC 50 — the VT parses no OSC 50
  (§0b), so nothing else can answer it.
* **`;` in a family name** would make the reply ambiguous for a strict parser. `fc-list` families
  do not normally contain `;`; the code does not escape it and that is recorded, not hidden.
* **Fragmenting.** The pty reader chunks arbitrarily; the scanner state is per-pane and persistent,
  so no reply is lost or duplicated across a chunk boundary.
* **Replay.** Answering replayed history would spawn phantom replies in fresh shells; §3.5 keeps
  `replay_pane_log` untouched and the plan's test does not exercise it.
* **The card's stated goal is not met by this change** — fastfetch will still print a system font
  or `Unknown terminal: ghostwave`, because it never asks (§0c). If the owner wants fastfetch to
  show the real font, that is an upstream fastfetch card (matcher entry for `ghostwave`) or a
  `terminalfont`-aware preset, and hermz should decide whether to file it. Nothing in ghostwave
  can do it.

## 7. NOT doing (explicit)

* No OSC 12 answer, no cursor-colour behaviour change.
* No XTGETTCAP / kitty-cap font query (Option B) unless stage 2 rules for it.
* No font *size* in the reply (`OSC 50` has one name field; size is a separate protocol).
* No `OSC 50` *set* support (changing the font at runtime) — the query only.
* No edits to `libghostty-vt` (pinned 0.2.1), no dependency changes.
* No push, no PR, no merge: branch + commit reported to hermz.

## 8. Ordered work items for stage 3

1. `git worktree`/branch `wt/t_c989ef4e` from `master` (already created at `.worktrees/t_c989ef4e`),
   confirm `git log -1` is `1a08166`.
2. Run the suite once through the gate and record the real baseline count (card says 216).
3. `src/ui_fonts.rs`: add `active_family()` (+ doc comment).
4. `src/font_query.rs`: `Scanner` + the six unit tests from §4.
5. `src/term.rs`: `font_query` field, `font_family` field, `scan_queries()`; call it in `poll_pty`;
   add the pane-level acceptance test.
6. `src/pane.rs`: call `scan_queries` + `flush_response()` in `ingest`.
7. `cargo fmt`/`clippy` as the repo does, full suite through the gate, paste output.
8. Update `PROGRESS.md` with the traced path and the premise correction (this repo keeps a running
   progress log — the `wt/osc7501` branch shows the shape).
9. Commit PLAN.md and the implementation separately (plan commit is already on the branch), report
   branch + commit hashes; **do not push**.
