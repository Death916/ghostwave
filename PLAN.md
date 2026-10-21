# PLAN — PageUp/PageDown in ghostwave (redo of reverted `4150de7`)

Card: t_9d3b6535 · repo: `/home/death916/code/ghostwave` · branch `wt/t_9d3b6535` off `master` `1a08166`
Baseline: `cargo test` on `master` = **211 passed / 0 failed**.

## 1. The complaint

"pg down/up dont work": at a shell prompt, with output in ghostwave's own
scrollback, pressing **PageUp / PageDown does nothing**. It is not broken code —
it is a design hole: plain page keys fall through to the shell, and only
**Shift+PageUp / Shift+PageDown** scroll ghostwave's scrollback.

The reverted patch (`4150de7`) "fixed" it by binding `PageUp`/`PageDown`
unconditionally in `normal_binds()`. It (a) did not compile — `match` arms mixing
`Action` and `Option<Action>`, `src/keymap.rs:394` — and (b), even if it had
compiled, it would have taken the page keys away from every full-screen program
(`less`, `vim`, `htop`, `git log`), because a Normal-mode bind is resolved
*before* the key is ever handed to the pty.

## 2. PageUp end-to-end today (`master` `1a08166`)

1. **Platform → code.** iced reports the *physical* key; `src/keys.rs:89-90` maps
   `Code::PageUp`/`Code::PageDown` to `Key::PageUp`/`Key::PageDown`, and
   `keys::keypress()` builds `KeyPress { key, mods: mods_from(modifiers), text }`.
   `main.rs:328` calls it; `main.rs:332` calls `input::key_press(.., Action::Press)`.
2. **Bind resolution.** `src/input.rs:64` → `keymap::handle(mode, kp)`.
   - `keymap.rs:349` `global_binds()` runs first. It matches **Shift**+PageUp
     (`keymap.rs:280`) → `Action::PageUp`. A **plain** PageUp matches nothing.
   - `keymap.rs:361` then dispatches on mode: `Mode::Normal => normal_binds()`.
   - `normal_binds()` (`keymap.rs:389-392`) returns `None` — deliberately:
     "zellij normal mode has no binds; everything passes".
   - `keymap.rs:376-380`: `None` + `Mode::Normal` → `Dispatch::Pass`.
3. **Pass.** `src/input.rs:80-94`: `content_key()` (content panes only → false for
   a terminal), the Shift+Arrow special case (not these keys), then `send()`.
4. **pty.** `input.rs:174-190` `send()` → `pane.key_press(kp, Press)`
   (`src/pane.rs:354`) → `TermPane::handle_key` (`src/term.rs:528-541`) encodes
   the key with libghostty-vt's key encoder into `TermPane::response` and flushes
   it to the pty master. For a primary-screen terminal with no application modes
   set, `PageUp` encodes as **`ESC [ 5 ~`** and `PageDown` as **`ESC [ 6 ~`**
   (`src/term.rs:536-538`).
5. **What the inner program does with it.** A shell/readline has no binding for
   `ESC[5~`: it reads the bytes and ignores them. The screen does not move —
   *that is the bug the user sees.* A full-screen program that has switched to
   the **alternate screen** (`less`, `vim`, `man`, `git log` via `less`) does
   have a binding and pages its own view: `less` scrolls one screenful.
6. **Ghostwave's own scrollback.** Only reached through an `Action`
   (`input.rs:389-398`: `Action::PageUp` → `pane.scroll_page(-1)` →
   `TermPane::scroll` → `TermViewport::Delta`, `src/term.rs:672-688`). Today
   that means **Shift+PageUp only**. In the alternate screen there is no
   scrollback to scroll, so Shift+PageUp there is a no-op.

**So:** with `less` running, plain PageUp already works (it reaches `less`);
at a shell, plain PageUp is read and discarded; and ghostwave's own scrollback
answers only to Shift+PageUp, which nothing in the UI advertises.

## 3. Decision

**The app's scroll handling wins — not a Normal-mode bind, and not Scroll mode.**

- `normal_binds()` stays empty. A bind there is resolved before the pty sees the
  key and can never reach the inner application; that is precisely the reverted
  patch's defect.
- Scroll mode (`Ctrl+s`, `scroll_binds()` `keymap.rs:474-475`) already maps
  PageUp/PageDown; it is unchanged and remains the explicit way in.
- The new rule lives in the **app layer** (`src/input.rs`), because only there
  does the dispatch path hold the pane's VT state — the bind table in
  `src/keymap.rs` is a pure function of `(key, mods, mode)` and cannot know
  whether the pane's program currently owns the screen.

The rule:

> In **Normal mode**, a **plain** (no-modifier) `PageUp`/`PageDown` aimed at a
> focused **terminal** pane that is on the **primary screen** is ghostwave's
> scrollback: it scrolls the pane a page. If the pane is on the **alternate
> screen**, the key is passed to the program unchanged.

Why that condition is the honest one:

- The primary screen *is* ghostwave's scrollback — that screen is the only one
  that has history. When a program is on the primary screen there is nothing in
  the program that can consume `ESC[5~` (readline, `cat`, `tail -f` and friends
  do not page), so claiming the key costs the user nothing.
- The alternate screen *is* the program's own view; `less`/`vim` own their page
  keys there, and there is no ghostwave scrollback to scroll anyway. Passing the
  key there keeps the pager working — the thing the reverted patch broke.
- Modifier variants are untouched: **Shift+PageUp/PageDown keep their existing
  global bind** and work in both screens (no-op in the alternate screen, which
  has no history), and `Ctrl+s` Scroll mode is unchanged.
- Escape hatch for the one residual case — a pager that stays on the primary
  screen (`less -X`): `Ctrl+g` **Locked** mode passes every key straight to the
  pane (`input.rs:39-44`, `keymap.rs:305-311`). It is documented here rather
  than silently losing the keys.

Consumed here also means consumed on **release** and on **repeat**: a press that
became a scroll must not leak its key-release into the pty (kitty
release-reporting would see a bare release), and holding the key should keep
scrolling instead of spamming the pty.

## 4. Work items (order is the commit order)

1. `src/term.rs`: add `TermPane::on_primary_screen() -> bool` — `matches!(
   self.term.active_screen(), Ok(Screen::Primary))`; a read error reports *not
   primary*, so the key is given to the program rather than taken from it.
   Import `libghostty_vt::screen::Screen`.
2. `src/input.rs`: add `fn scrollback_page_key(state: &AppState, kp: &KeyPress)
   -> Option<Action>` — plain mods only; `PageUp => Action::PageUp`,
   `PageDown => Action::PageDown`; focused entry must be `PaneEntry::Term`; and
   that pane must be on the primary screen. One predicate, three call sites, so
   press/release/repeat can never disagree.
3. `src/input.rs` `key_press`: in the `Dispatch::Pass` arm, before the Shift+Arrow
   special case, consume it as an `Action`.
4. `src/input.rs` `repeat`: same consumption, before `send()`.
5. `src/input.rs` `key_release`: return without sending the release when the same
   predicate holds.
6. Tests (see §5). No test-only production hooks.

## 5. Verification (commands and expected results)

All builds through the gate, one target dir for this card:

```
CARGO_TARGET_DIR=/var/build/ghostwave-t_9d3b6535 \
BUILD_GATE_WAIT=900 bash ~/.hermes/scripts/build_gate.sh cargo test
```

- **RED, before item 2-3:** the new `plain_page_keys_scroll_ghostwaves_own_scrollback`
  fails (`assert!(after.offset < before.offset)`), proving the complaint on code.
- **GREEN, after:** whole suite passes: **211 + 5 new = 216 passed / 0 failed**
  (52 lib + 157 bin + 1 + 6; 5 new: plain page keys scroll the scrollback,
  a held page key keeps scrolling, the alternate screen keeps the key, the pty
  encoding of PageUp/PageDown is `ESC[5~`/`ESC[6~`, and the alternate-screen
  flag drives `on_primary_screen()`; the keymap test `normal_unbound_passes_to_terminal`
  gains the page-key assertions).
- `git -C /home/death916/code/ghostwave rev-parse master` still `1a08166`;
  `git status` clean on `master`.

What the tests assert (the behaviour claim, mechanically):

| pane's screen | key | what happens | how the test sees it |
|---|---|---|---|
| primary (`bash`) | plain PageUp | ghostwave scrollback scrolls up a page | `scrollbar().offset` decreases |
| primary (`bash`) | plain PageDown | scrolls back down | offset increases |
| primary | held PageUp (`repeat`) | keeps scrolling | offset decreases |
| alternate (`less`, after `ESC[?1049h`) | plain PageUp | **not** consumed; `ESC[5~` goes to the pty | offset unchanged + encoder test `response == b"\x1b[5~"` |
| any | Shift+PageUp | unchanged global bind | existing test `scrolling_and_clear_reach_the_pane` |

## 6. Scope — what is NOT done

- No change to `normal_binds()`, `scroll_binds()`, or any other bind table entry.
- No Shift+PageUp/PageDown, Ctrl+f/Ctrl+b, or Ctrl+s change.
- No mouse-wheel or scrollbar-drag change; no "type-to-snap-to-bottom" change.
- No config option for the new rule (`scrollback_page_keys = ...`); if the owner
  wants it off, that is a separate card.
- No rebind of `Home`/`End`/`Insert`/`Delete`.

## 7. Risks

- **Primary-screen pagers** (`less -X`, `less` when its output is not a tty) lose
  plain PageUp. Accepted and documented; `Ctrl+g` Locked mode still delivers the
  raw key. Every other mainstream pager uses the alternate screen.
- **Pane lock**: the rule locks the focused pane's mutex to read its screen. It
  is the same lock `send()` and `key_release()` already take on this path, and it
  is released before `execute()` runs.
- **Screen-read race**: a program can switch screens between the read and the
  key reaching the pty; worst case one page key goes the wrong way, self-corrects
  on the next press.
- **A pane that is not a terminal** (preview/AI/visualizer) is untouched: the
  content-pane path (`content_key`) already handles page keys and runs first.

---

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

---

# PLAN — durable sessions as their own type, and a readable durable marker in the session list

Card: t_a4284019 · branch `wt/t_a4284019` · worktree `/home/death916/code/ghostwave/.worktrees/t_a4284019`
Repo: ghostwave (iced front-end + core lib). Base commit: `1a08166`.

## 0. The owner's report, verbatim

> i started a session with ctrl shift p then ssh orac. this was not durable as when i rejoined it it was a new
> shell. we need: 1 durable sessions to be there own type and command and if they are already it should show in
> the command pane when typing durable. and 2 it should show in the session list if its durable or not by a
> label or highlight or something else

### 0.1 Owner follow-up (2026-10-21, after the first plan revision)

> sorry the ds command does work jsut needs to be more obvious

This settles the one thing the plan had left open and narrows the job to discoverability:

- `ds <host>` already produces a durable session — its behaviour is **not** changed by this plan (§3 D3 keeps the
  `ds ` item's *title and action* byte-identical; only its category becomes `Remote — durable`, and §4.4 requires
  the existing `dynamic_ds_prefix` test to pass unmodified — it asserts titles only).
- The failure was that nothing in the palette or the session list made the difference *visible*: `ssh orac` and
  `ds orac` looked the same, and a durable session was badged `ssh`. So the work is labels + the durable
  inventory, which is what §3 D3/D5/D6 do.
- Consequence for §7: the owner has now said the two commands stay; this plan must not re-litigate that. §7 is
  closed as "keep both, make durable obvious".

## 1. Root cause (verified by reading the code, not assumed)

Two independent defects, one per numbered want.

**(a) The palette cannot show that a durable session already exists, and cannot show that `ssh` is not durable.**
`ssh <host>` (`src/app.rs:1061` `connect_ssh`) spawns a plain `ssh` pty with `remote: None`; the remote shell
dies with the connection. Durable is a *different* command, `ds <host>` (`src/app.rs:1087`
`connect_durable_ssh`), which runs `ssh -t <host> … ghostd attach ghostwave-<host>` and binds the pane to a
`RemoteSpec`. The palette (`src/palette.rs:80` `filtered_items`) knows both prefixes, but:

- `ds <host>` only ever builds **one** item for the literal text typed — the already-known durable sessions are
  never listed, so there is nothing to "show when typing durable";
- the plain item is titled `Connect SSH: <host>` in category `Remote`, exactly like the durable one, so nothing
  in the palette says which one survives a reconnect (this is how orac was opened non-durably);
- `PaletteState` is built from a static inventory at `src/input.rs:459` (`default_items(&theme::names())`) — it
  has no channel for runtime data at all.

**(b) The session list's marker means the opposite of what it says, and plain ssh has no marker.**
`session::has_remote` (`src/session.rs:151`) is true only for panes with `remote.is_some()` — i.e. durable. The
picker renders that as a lowercase **`ssh`** badge (`src/sessions_overlay.rs:198-212`). So a durable session is
labelled `ssh`, and a plain `ssh <host>` session — the thing that bit the owner — has **no marker at all** and is
indistinguishable from a local session in `Session Manager`.

## 2. Scope — and what is deliberately NOT done

In scope: `src/session.rs`, `src/sessions_overlay.rs`, `src/ui_session.rs`, `src/palette.rs`, `src/input.rs`,
`src/app.rs` (one tab-title string). No new files in `src/`.

NOT done (each is a real gap, called out so review does not read silence as coverage):

- no ghostd change, no daemon protocol change;
- no change to what any existing command *does*: `ds <host>` and `ssh <host>` keep their exact behaviour and
  their existing palette titles (owner, §0.1) — this card is labels + inventory only;
- no new on-disk file/format: no "known hosts" history (see D1 for why);
- no auto-connect on restore — `remote.rs`'s invariant ("a remote pane is never auto-connected on restore") holds;
- no change to `PaneState` / `RemoteSpec` / session JSON: files stay readable by older builds;
- no status-bar / tab-bar badge work beyond D6's tab title; no hover tooltips, no new modal;
- no change to the pane label band: `src/pane_canvas.rs:124-140` (`remote_status`) renders
  `ssh <host> — reconnecting (attempt N) — Esc to stop` and `ssh <host> — disconnected (Enter connect | k kill)`
  for a durable pane, so a durable session is spelled `ssh` there too — the same misleading label D5/D6 fix in
  the picker and the tab, in a second render site. Not fixed by this plan: out of the sanctioned file set;
- the README "Not in this build" section is already stale (remote/SSH is ported) — left alone, not this card.

## 3. Binding design decisions

**D1 — the durable inventory is read from disk + live panes, not from a new file.**
Sources: new `ghostwave::session::all_remote_specs() -> Vec<(String, String)>` (every saved session's
`(host, daemon session)` pairs, in session-name order) merged with the current workspace's live
`AppState::live_remote_specs(&tabs)` (`src/app.rs:1249`), deduped by host with the existing
`remote::dedupe_hosts` (`src/remote.rs:223`), sorted by host. Rationale: the picker already derives its badges
from the session files ("the current session is written every two seconds", `src/ui_session.rs:47`), and a
durable session that has been killed or deleted no longer exists — a persisted host list would offer targets
that are gone. Cost: one small JSON read per saved session, once per palette open, on the input path.

**D2 — the palette is handed the durable targets, it does not read the disk itself.**
Same rule `SessionsOverlay` already follows (`src/sessions_overlay.rs:1-8`): a widget that reads the data dir
cannot be tested without touching it. `PaletteState` gains `durables: Vec<(String, String)>` (host, daemon
session) and a constructor `PaletteState::with_durables(items, durables)`; `PaletteState::new(items)` keeps its
signature and means "no durable targets", so every existing palette test compiles unchanged.

**D3 — query grammar in `filtered_items` (trimmed, case-insensitive on the prefix).**
Let `q = self.query.trim()`, `lower = q.to_lowercase()`.

| query | items returned |
|---|---|
| `ds <host>` (`lower.starts_with("ds ")`, non-empty host) | title and action **unchanged** (`Durable SSH (ghostd): <host>`, `ConnectDurableSsh(host)`); category becomes `Remote — durable` (today `Remote`, `src/palette.rs:115`) |
| `durable` bare (`lower == "durable"`) | one item per known target, **no create item** (a create needs a host): title `Durable session: <host> — <daemon session>`, category `Remote — durable`, `ConnectDurableSsh(host)`; if the inventory is **empty**, fall through to normal fuzzy filtering |
| `durable <text>` (`lower.starts_with("durable ")`) | known targets whose host or daemon session contains `<text>` (case-insensitive), in host order; then, when `<text>` is non-empty, one extra `New durable SSH (ghostd): <text>` (`Remote — durable`, `ConnectDurableSsh(<text>)`) |
| `ssh <host>` | `Connect SSH: <host>` with category `Remote — not durable` (title byte-identical, so the existing test holds), **plus** — only when a known target's host equals `<host>` — `Durable session: <host> — <session>` (`Remote — durable`, `ConnectDurableSsh(host)`) |
| anything else | unchanged fuzzy scoring |

The empty-inventory fall-through for bare `durable` is deliberate: with nothing to list, the existing
"No matching commands" hint is the honest answer, and the hint text is updated (D7) to say how to create one.

**D4 — one core predicate for the three states.** In `src/session.rs`:

```rust
/// Whether a saved session is a durable (ghostd-bound) remote session, a plain
/// ssh one, or neither.
pub enum RemoteKind { Durable, PlainSsh }

/// The remote kind of `name`: `Durable` when any pane carries a [`RemoteSpec`],
/// else `PlainSsh` when any pane's program is `ssh` (how `connect_ssh` spawns
/// one), else `None`, and `None` for a session that does not load.
pub fn remote_kind(name: &str) -> Option<RemoteKind>
```

plus `pub fn all_remote_specs() -> Vec<(String, String)>`. A durable pane wins over a plain one in a mixed
session (`Durable` covers `PlainSsh`). Detection of a plain pane is exact, not heuristic: `connect_ssh` spawns
program `"ssh"` with `remote: None`, and the saved `PaneState.program` is that same string.
`has_remote(name)` stays, re-implemented as `remote_kind(name) == Some(RemoteKind::Durable)`, so its only
caller, `src/ui_session.rs:55` (`remote_names`'s filter), keeps working.

**D5 — the picker carries kinds, not a set.** `SessionsOverlay.remotes: HashSet<String>` becomes
`kinds: HashMap<String, RemoteKind>`; `SessionsOverlay::new(current, names, kinds)`; `is_remote` is replaced by
`is_durable(name)` and `kind(name) -> Option<RemoteKind>`; `relist(names, kinds)`. `ui_session::remote_names()`
becomes `remote_kinds() -> HashMap<String, RemoteKind>` and its three call sites (`open_picker`,
`refresh_picker`, the delete branch of `picker_message`) pass the map through.

Badges in `view` (`src/sessions_overlay.rs:198-212`): `Durable` → `ds` in `theme.search_match` (the highlight
colour the current `ssh` badge already uses), `PlainSsh` → `ssh` in `theme.tab_inactive_text` (dim). The header
line becomes `SESSIONS — current: <name> (durable)` / `(ssh)` / `<name>` so the highlighted row is not the only
place the state is readable. The key-hint line gets no change.

**D6 — the tab title names the type.** `connect_durable_ssh` titles its tab `ds <host>` (today `ssh <host>`,
`src/app.rs:1115`) while plain `connect_ssh` keeps `ssh <host>`. The *session* name stays `ssh-<host>`
(`remote::ssh_session`) for both — that is the documented reason a repeated `ds`/`ssh` on the same host focuses
the existing session instead of stacking a duplicate (`src/remote.rs:70-79`), and it must not drift.

**D7 — the palette's empty hint** becomes
`No matching commands (try: open <path> | ssh <host> | ds <host> | durable | theme <name>)` — today's string
(`src/palette.rs:472`) with the two durable forms inserted and `theme <name>` **kept**: dropping an existing
affordance is not this card.

## 4. Ordered work items (stage 3 executes these in this order)

1. `src/session.rs` — add `RemoteKind`, `remote_kind`, `all_remote_specs`, `has_remote` on top of `remote_kind`.
   `all_remote_specs` is a thin fan-out over the **existing** `session::remote_specs(name)` (`src/session.rs:157`,
   today per-session, used by the manager's kill path) over `list_sessions()` (`src/session.rs:113`, already the
   enumerator behind `core::list_sessions()` at `src/ui_session.rs:43`) — reuse it rather than re-walking the JSON.
   Tests (in the existing `mod tests`, which runs against the real data dir with unique names + cleanup —
   see §6 Risks): `a_durable_pane_makes_the_session_durable`, `a_plain_ssh_pane_is_ssh_and_not_durable`,
   `neither_pane_kind_is_neither` (`remote_kind` → `None`), `all_remote_specs_lists_a_durable_binding`
   (assert `contains`, never list equality — the real dir may hold the owner's sessions).
2. `src/ui_session.rs` — `remote_kinds()`, and the three call sites (`refresh_picker` :62, `open_picker`
   :249, the delete branch of `picker_message` :283). Rename/retarget the test
   `a_remote_pane_comes_back_disconnected_and_its_session_is_badged_ssh` (:561) → `…_is_badged_durable`, and in
   the same test retarget **both** `remote_names()` assertions — :586 `remote_names().contains(&name)` and :589
   `names().contains(&name) || remote_names().contains(&name)` — onto `remote_kinds()`
   (`remote_kinds().get(&name) == Some(&RemoteKind::Durable)`, and the :589 fallback likewise).
3. `src/sessions_overlay.rs` — `kinds: HashMap<String, RemoteKind>`, `is_durable`, `kind`, badges, header.
   The third argument of `new`/`relist` is constructed at **four** sites in this file and every one of them changes
   (`remotes: HashSet<String>` → `kinds: HashMap<String, RemoteKind>`); the tree's other three third-arg sites are
   the production calls in `src/ui_session.rs` (:64, :249, :285 — §4.2). Plus the four `is_remote` assertions:

   | site | line | edit |
   |---|---|---|
   | `picker()` test helper | 331-338 | `HashSet::new()` → `HashMap::new()` (mechanical) |
   | `the_current_session_is_listed_and_selected` | 346 | `!is_remote("default")` → `!is_durable("default")` |
   | `a_session_with_a_durable_remote_pane_is_badged_ssh` → `…_is_badged_ds` | 351-357 | `HashSet::from(["ds-prod"])` → `HashMap::from([("ds-prod", Durable)])`; both `is_remote` → `is_durable` |
   | `an_unknown_current_session_is_added_to_the_list` | 362 | `HashSet::new()` → `HashMap::new()` (mechanical) |
   | `a_relist_keeps_the_selection_on_the_current_session` | 440-446 | `relist` third arg `HashSet::from(["a"])` → `HashMap::from([("a", Durable)])`; 446 `is_remote` → `is_durable` |

   Add `a_plain_ssh_session_is_badged_ssh_and_a_durable_one_ds` and `a_mixed_session_with_a_durable_pane_is_durable`.
4. `src/palette.rs` — `durables` field, `with_durables`, the D3 grammar, the D3 category strings, the D7 hint.
   Keep `dynamic_ssh_prefix` and `dynamic_ds_prefix` passing **unmodified** (byte-identical titles/actions).
   New tests: `typing_durable_lists_the_known_durable_sessions`,
   `durable_with_a_host_offers_the_new_session_below_the_known_ones`,
   `ssh_beside_a_known_durable_target_offers_the_durable_session`,
   `a_bare_durable_with_an_empty_inventory_lists_nothing_durable`.
5. `src/input.rs` — `Action::OpenCommandBar` builds the targets
   (`remote::dedupe_hosts(live + session::all_remote_specs())`, sorted by host) and calls
   `PaletteState::with_durables(...)`. This is the *action* path, never the render path: `live_remote_specs`
   takes pane mutexes (`src/app.rs:1249-1259`) and `view` must not block on them.
   Update `the_durable_ssh_action_opens_its_own_session_and_binds_the_pane` (`src/input.rs:2199`) to expect tab
   name `ds <host>`; in `the_plain_ssh_action_opens_its_session_without_a_ghostd_binding` add
   `assert_eq!(s.current().name, format!("ssh {TEST_HOST}"))`. Add
   `the_palette_lists_the_durable_sessions_it_was_given` (open, type `durable`, expect the item + action).
6. `src/app.rs` — the D6 tab title (`src/app.rs:1115` only). No app.rs test asserts that string today
   (`src/app.rs:1578` is a test *helper* that names a restored tab `ssh {host}`; leave it, it is just a name).

## 5. Verification — exact commands, expected results

Gate every compile (mandatory, orac is 4 cores / shared):

```sh
cd /home/death916/code/ghostwave/.worktrees/t_a4284019
export CARGO_TARGET_DIR=/var/build/ghostwave/t_a4284019        # per-card; never a shared target dir
bash ~/.hermes/scripts/build_gate.sh nix-shell ./shell.nix --run 'cargo test --all-targets --no-fail-fast'
```

- exit 0 **and** `build_gate.sh` exit 0. Exit 3 from the gate = lock held → comment + block `transient`, do not
  compile anyway. Exit 4 = low RAM → retry later.
- expected: every `test result: ok`, zero `FAILED`, for the four targets the suite runs: `--lib` (core),
  the bin unittests (`src/main.rs` + modules), `tests/flood_throughput.rs`, `tests/term_core.rs`.
  Baseline count at `1a08166`: 204 `#[test]` in `src/` + 7 in `tests/` (measured by `grep -c '#\[test\]'`).
- **The real edit set, so stage 3 knows what must move** (no test is deleted; every edit below is either a
  signature ripple or an assertion retarget, and any *other* test that needs touching is a plan defect — report
  it, do not silently widen the diff):
  - `src/sessions_overlay.rs`: the third argument of `new`/`relist` is constructed at **four** sites
    (`picker()` :331-338, `an_unknown_current_session_is_added_to_the_list` :362 — both mechanical
    `HashSet::new()` → `HashMap::new()`; `…_is_badged_ds` :351-355, `a_relist_…` :440-443 — value changes), and
    the four `is_remote` assertions (:346, :356, :357, :446) become `is_durable`;
  - `src/ui_session.rs`: `a_remote_pane_comes_back_disconnected_and_its_session_is_badged_ssh` is renamed and
    both of its `remote_names()` assertions (:586, :589) move to `remote_kinds()`;
  - `src/input.rs`: `the_durable_ssh_action_opens_its_own_session_and_binds_the_pane` (:2199, tab `ds <host>`)
    and the added assertion in `the_plain_ssh_action_opens_its_session_without_a_ghostd_binding` (§4.5).
  - Count: 204 → **215** `#[test]` in `src/` (11 new: 4 in `session.rs`, 4 in `palette.rs`, 2 in
    `sessions_overlay.rs`, 1 in `input.rs`); `tests/` stays 7. Stage 3 must paste the real per-target counts and
    reconcile any difference.
- a fresh `CARGO_TARGET_DIR` recompiles libghostty-vt with zig (`shell.nix` pins zig 0.15.2); `/var/build` had
  25 GB free at plan time, the last target dir cost 5.3 GB. Use `nix-shell ./shell.nix` — **not** a minimal
  shell (PROGRESS.md:643 records why).
- **Do NOT run `cargo fmt --check`**: measured on this tree, it already fails at `1a08166` with pre-existing
  diffs in `src/` and `tests/term_core.rs` (FMT_EXIT=1 before any edit). Keep hand formatting consistent with
  the surrounding code instead.
- Manual GUI check (badge colour, the `durable` list on screen) needs a Wayland session and is the owner's
  local test — the PR body must say so; do not claim it.

## 6. Risks

- **Lock ordering.** `live_remote_specs` locks each pane mutex. It is called from the palette-open action only
  (D2/§4.5), never from `view`. Two panes cannot deadlock here (sequential, one at a time), but a future caller
  inside the render path would.
- **Test hermeticity.** `src/session.rs`'s own tests run against the **real** `~/.local/share/ghostwave` (only
  `ui_session::test_lock` — in the *bin* test binary — redirects `XDG_DATA_HOME`). So the lib tests must use
  unique names, must assert with `contains`, and must clean up; and the palette must never read the disk in a
  test path (that is what D2 buys).
- **Prefix parsing.** `lower == "durable" || lower.starts_with("durable ")` must be checked before the generic
  fuzzy path, and `"ds "` must stay a separate prefix (a query `ds` with no space keeps falling through, as it
  does today). A sloppy `starts_with("ds")` would swallow `dsomething`, and `starts_with("durable")` would
  swallow a future `durable-x` command.
- **Category strings are fuzzy-matched**, so renaming `Remote` → `Remote — durable` / `Remote — not durable`
  changes what a query like `remote` matches. No test asserts on those categories today; the new tests do.
- **Discoverability, not enforcement.** After this change the palette *shows* the difference and offers the
  durable alternative; it does not stop anyone choosing plain `ssh`. Making `ssh` durable-by-default is a
  product decision that belongs to the owner — out of scope here (see §7).
- Badge colour choice reuses existing theme roles (`search_match`, `tab_inactive_text`), so no theme file,
  `Config` or `Theme` change is needed and themes without the role cannot regress.

## 7. The `ssh` question — closed by the owner (§0.1)

Both commands stay: `ssh <host>` keeps spawning a plain ssh pty, `ds <host>` keeps spawning the durable one.
The plan only *labels* them and offers the durable twin next to a plain `ssh <host>` when a durable session for
that host is already known (§3 D3). No command is renamed, removed, or made durable by default, and no
`ssh-plain` alias is introduced — the owner has explicitly said `ds` works and the gap is discoverability.
