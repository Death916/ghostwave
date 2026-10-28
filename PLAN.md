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

---

# PLAN — a pane's canvas must not paint over its frame stroke

relay: job=t_b6e8cb0c · stage 1 (PLAN), **revision 3** (this card: `t_142a44ce`) · stage-2 verdict
that produced it: changes-required on `t_69b8f4cd` (round 2) · branch `wt/t_b6e8cb0c` (based on
`master` @ `a9c2f97`) · owner report: "pane still overlaps border. the canvas seems to go over the
border line but you can see a small bit of it on the left" · screenshot `https://ibb.co/VWvPmKyj`
· **owner answer to §9 arrived mid-review, quoted verbatim in §9**

This card arrived from the dashboard with **no body and no plan stage bought**; the previous card in
this series (`t_2a5f5dab`, PR #16, merged as `46c1ce1`) fixed the *missing* frame line where two
panes touch. This one is the residual report: the frame line is there, but a pane's **canvas paints
over it**. *Revision 3 corrects that reading with the owner's own answer (§9): the covered stroke is
on the edge two **stacked** panes share, which §1.3 shows no canvas paint can reach — so this plan
fixes a real overhang defect that is **not** the owner's report, and says so everywhere.*

## Revision 3 — what changed (this card `t_142a44ce`)

Stage 2 round 2 (`t_69b8f4cd`) accepted the geometry and the clip finding and returned three
blocking defects plus four minors. Revision 3 is those fixes and nothing else: **no measured number
from revision 2 changed**, and no design change follows from the fixes except one extra test.

1. **B1 — §5's full-suite expectation was false.** Revision 2 said `220 passed` / "218 today". Enumerated
   here, at the base commit `a9c2f97` (the two plan commits touch PLAN.md only):

   ```
   grep -rho '#\[test\]' src/   | wc -l   → 252      (72 lib-target + 180 bin-target modules)
   grep -rho '#\[test\]' tests/ | wc -l   →   7      (flood_throughput 1, term_core 6)
   #[ignore] anywhere / cfg(feature in src/ / doctest code fences in the lib modules → 0 / 0 / 0
   ```

   **259 test functions**, not 218. The 218 was PR #16's tree (`46c1ce1`) — two merges stale
   (`c8b7404` +24, `a9c2f97` +17), and the same stale `PROGRESS.md:1377` source that revision 1 got
   its count from. §5 now says *measure the base run, then expect exactly +3* (262), and separately
   why the increment is +3 and not the +2 stage 2 counted: B2 adds a third test (§4).
2. **B2 — the shared edge, which is where the owner's report lives, is now computed.** New §1.3
   derives the gap from a pane's painted fill to its own and its neighbour's stroke at a stacked
   split's boundary, states why §1.1's fill row and §1.2's marker threshold are unchanged there, and
   pins the painted excursion with a new test. The layout-level half is already pinned
   (`framed_panes_carry_their_frame_on_shared_edges`); §1.3 says so rather than duplicating it.
3. **B3 — §9's question is answered, and the answer refutes revision 2's mapping.** The owner's
   sentence is quoted verbatim in §9. It places the covered stroke at the **shared edge of a
   vertically stacked split**, which §1.3 proves is not something `paint_pane` can paint. §9's "a
   pane frame → the clamp is the whole fix" mapping is replaced by the truth: **stage 3 does not
   address the reported symptom under any configuration this plan computes.** The symptom becomes
   its own card — first lead PR #16 (`46c1ce1`), merged against this same report — with the owner's
   `font_size` / `padding` / `show_frames` and an unscaled capture still owed (§9).
4. **Minors, applied verbatim.** M1: §1.1's cursor row is now an explicit *assumption* with the FFI
   citation and the font size at which it would matter. M2: §1.2's `padding = 0` case opens at
   `font_size ≥ 6`, not "any font". M3: the `font_size 12` row says the marker **is** drawn — in the
   gutter, on screen, simply not touching a stroke — which is what §3's no-clip finding implies.
   M4: the `NEEDS-OWNER-VERIFY` move is stated canvas-local (`−1.2 → 0`), the only form that holds
   for focused and unfocused panes alike.

## Revision 2 — what changed, and against what evidence

1. **Withdrawn: §1's old causal claim.** Revision 1 said the `origin_offset` paint rect can extend
   the pane's background fill "up to 0.5 px past the canvas onto the cell's stroke". It cannot: with
   frames on the canvas edge is `inset = 4.0` px inside the stroke's inner edge, and a ≤0.5 px
   overhang lands in that gap, which is painted the same `theme.bg` the fill uses. Zero visible pixel
   in every configuration (§1.1). Stage 2 was right and revision 1 was wrong.
2. **Added: what actually leaves the canvas.** `paint_pane` paints five things; four are provably
   inside it, and one — the search rows' left gutter marker — leaves it by up to
   `cell.w − config.padding` px and does reach the stroke, but only when a cell is wider than the
   gutter (§1.2). That threshold is now measured, not asserted: `cw = 0.600 · font_size` from the
   bundled face, so the marker's path to a stroke opens at `font_size ≥ 16` (unfavourable layout
   phase) / `≥ 18` (any phase) at the default `padding = 6.0` — and at `font_size ≥ 6` if the owner
   runs `padding = 0` (revision 3 corrects this sentence in §1.2; that claim of "any font" was loose).
3. **Withdrawn: the whole-paint `with_clip`.** Stage 2 called it a crop of the marker; measured, it
   is worse — at the defaults the marker's ink lies **entirely outside** the canvas
   (canvas-local `−1.2 … −0.3`), visible only because iced gives the canvas no clip, so
   `with_clip(ORIGIN .. bounds.size())` around `paint_pane` would **delete the search marker**. The
   clip is replaced by a clamp on the one primitive that leaves (§3).
4. **§9 is asked with this card**, not after stage 3: at the defaults nothing in `paint_pane` touches
   a stroke, so the screenshot's element is still unidentified and only the owner can identify it.

§5 is still the acceptance; §10 now lists what stage 2 has to judge **revision 3** on.

## 0. PREMISE CHECK (measured, not assumed — unchanged from revision 1, re-verified by stage 2)

* `https://ibb.co/VWvPmKyj` → image `https://i.ibb.co/0jfdtwND/image.png`, HTTP 200, **1492x922**.
  Decoded with a stdlib PNG reader and measured column-by-column (scripts kept at
  `~/.hermes/profiles/dev/cache/scratch/{pngtool,peaks,colprof,rgb2}.py`). Stage 2 re-fetched and
  re-measured it independently and got the same numbers.
* The image is a **downscaled crop of a screen, not a window capture**: every 1 px feature is a
  3–4 px luminance ramp; the only full-height bright line, `x 1424..1427`, ramps
  `(65,61,73) → (130,101,57) → (109,82,51)`.
* **No theme frame colour appears in the image.** Dracula's active stroke `#bd93f9` → 0 px at a ±20
  tolerance; tokyonight `#7aa2f7` likewise 0 px. `theme.frame` (`#44475a` for dracula) matches
  13 996 px, but they are glyph antialiasing scattered over the text area, not a stroke.
* The one full-height line at `x≈1424–1427` is **warm brown/orange** — dracula `search_match`
  `#ffb86c` blended ~50 % with the pane background, not a frame colour.
* Nothing in `master` draws a persistent right-hand panel: `src/main.rs:610-650` is a `Column` of
  tab bar / pane grid / status bar, with floats, toast, the search overlay
  (`OverlayPosition::TopRight`) and the *centred* sessions modal stacked above.
* This tree has **no scrollbar painting at all** (`scrollbar()` in `src/term.rs` is the VT's
  scroll-offset query, not a widget), so the right-edge line is not a scrollbar either.

**Verdict: PREMISE UNRESOLVED for the exact element — and revision 2 adds that no `paint_pane`
primitive can draw a full-height line, so the screenshot's element is not inside a pane's canvas at
all.** §1.2's marker is one row tall and `cw/8` wide; it cannot produce `x≈1424–1427`. What *is*
provable is a real overhang defect in `paint_pane` (§1.2); it is not necessarily the owner's.

Revision 3 adds the owner's own answer (§9, verbatim): the covered stroke is the **shared edge of a
two-pane stacked split**. That is the same conclusion from the other end — §1.3 computes that edge
and shows `paint_pane` stops ≥ 3.5 px short of even the pane's *own* stroke there, so the report is
not this diff's defect and not this diff's fix.

## 1. The geometry, from the constants up (traced, not inferred)

Given for frames on: `PANE_GAP = 4.0` (`src/panes_grid.rs:49`), `PANE_INSET = 2.0` (`:52`),
`SPACING = 0.0` (`:98`), `FRAME_ACTIVE_W = 2.0` / `FRAME_INACTIVE_W = 1.0`
(`src/pane_canvas.rs:23-25`):

* `inset(frames)` = `PANE_INSET + PANE_GAP/2` = **4.0 px** (`src/panes_grid.rs:113-119`); `0.0` with
  frames off.
* `cell_metrics` = `{ frame: 1.0 unfocused / 2.0 focused, padding: inset + frame }` = **5.0 / 6.0
  px** (`src/panes_grid.rs:135-146`).
* The terminal body is `container(Canvas::new(PaneCanvas).width(Fill).height(Fill)).padding(metrics.padding)`
  (`src/panes_grid.rs:202-221`), and iced hands `Canvas::draw` that child's layout bounds
  (`iced_widget-0.14.2/src/canvas.rs:291`, `let bounds = layout.bounds();`). So **canvas-local 0 is
  `metrics.padding` px inside the cell**, and the stroke — the `Content` border on the cell's
  outermost `frame` px (`src/panes_grid.rs:242-250`) — has its **inner edge `inset = 4.0` px outside
  the canvas**.
* The region between the canvas edge and the stroke is painted `theme.bg` by `Content::style`
  (`src/panes_grid.rs:243`), the same colour `paint_pane` fills with
  (`default_bg = rgb(theme.bg)`, `src/pane_canvas.rs:178,180-184`).
* The painter's own `padding` is **not** `metrics.padding`: `PaneCanvas.padding` is
  `state.config.padding` (`src/panes_grid.rs:189`, `src/main.rs:550`, default `6.0` at
  `src/config.rs:161`), so text and markers are laid out against `config.padding` inside a canvas
  that is inset by `metrics.padding`. At the defaults that is 6.0 vs 5.0 unfocused / 6.0 focused —
  a 1 px disagreement in unfocused panes, noted in §7 as out of scope.
* `rect` = the paint rect: `origin_offset = bounds.round() - bounds`, `pane_rect = Rectangle::new(origin_offset, bounds.size())`
  (`src/panes_grid.rs:502-503`). So in canvas-local coordinates `rect.x ∈ (−0.5, 0.5]` and
  `rect.x + rect.width = bounds.width + rect.x` — up to 0.5 px past the right/bottom edge and up to
  0.5 px before the left/top edge.

### 1.1 Every primitive `paint_pane` paints, against those numbers

`cw = cell.w`, `p = config.padding` (6.0 default), `rect.x ∈ (−0.5, 0.5]`.

| primitive | code | horizontal extent, canvas-local | can it reach the stroke (inner edge at `−4.0`)? |
|---|---|---|---|
| background fill | `src/pane_canvas.rs:180-184` | `rect.x … rect.x + width` = `−0.5 … width + 0.5` | **no** — 4.0 px of `theme.bg` gap between the canvas edge and the stroke, and the fill is `theme.bg`: zero visible pixel, all four edges, every configuration |
| cell background / glyph / underline / strike | `src/pane_canvas.rs:234-266` | `x = ox + col·cw` for `col < cols`, `cols = ⌊(w − 2p)/cw⌋` (`src/panes_grid.rs:61-68`) → the last ink ends at `rect.x + w − p` | **no** — ≥ 5.5 px inside the right edge; the same arithmetic bounds rows at the bottom |
| cursor block / bar / underline | `src/pane_canvas.rs:302-320` | `ox + vp.x·cw`, ink `… + cw`; `vp.x < cols` is an **assumption**, see below | **no, on that assumption** — inside the text area |
| **search-row left marker** | `src/pane_canvas.rs:285-295` | `x = ox − cw = rect.x + p − cw`, ink `x … x + cw/8` | **yes**, when `cw > p + inset + rect.x` |

**M1 — the cursor row's "no" is an assumption, not a measured invariant, and it is written down as
one.** `vp` is `snapshot.cursor_viewport()`, used at `src/pane_canvas.rs:302-305` as
`cx = ox + vp.x as f32 * cw` with no clamp. That call hands libghostty's FFI values straight
through: `libghostty-vt-0.2.1/src/render.rs:495-505` reads `CURSOR_VIEWPORT_X` and constructs
`CursorViewport { x, .. }` unchanged, and the only contract in the tree is the binding's field
comment, `libghostty-vt-sys-0.2.1/src/bindings.rs:2797-2798` — "Cursor viewport x position in cells
(uint16_t). Only valid when CURSOR_VIEWPORT_HAS_VALUE is true." The C/Zig side that would have to
guarantee `x < cols` is **not vendored** in either crate, so `vp.x < cols` cannot be verified from
this tree; it is assumed, on the reading that a *viewport* position is a position inside the
viewport. If `x == cols` ever arrives (pending-wrap is the usual way), the block's ink right edge is
`rect.x + p + (vp.x + 1)·cw`, which passes the canvas edge by up to `cw − p` and reaches the stroke
at `cw > p + inset` — worst phase `font_size ≥ 17` at the defaults, the marker's defect class one
axis over. (At the defaults it would already pass the canvas edge, but into the `theme.bg` gap —
invisible, same colour — so the visible symptom needs the stroke, i.e. `font_size ≥ 17`.) Round 2
estimated `≥ 21`; same class, different truncation case. Either way it is not
this diff's defect: it is recorded here so a later card starts from an assumption instead of from
the false "no" revision 2 shipped.

With frames off (`inset = 0`, `metrics.padding = 0`, `SPACING = 0`) the canvas fills the cell and the
≤0.5 px overhang lands on the neighbour's own `theme.bg` area, which carries no ink before
`cell-local p = 6` — still zero visible pixel, and there is no stroke to cover.

### 1.2 The marker is the only path, and its threshold is measured

`cw` is not free: `cell_dims` measures `font.metrics('M', font_px).advance_width`
(`src/pane.rs:803-816`) from the primary face. For the bundled `assets/JetBrainsMono-Medium.ttf`,
read straight out of the TTF (upem 1000; advance 600 units for `M`, `█` U+2588 *and* `▏` U+258F;
script `~/.hermes/profiles/dev/cache/scratch/fontadv.py`):

```
upem=1000  'M' advance=600 → ratio 0.600
font_size 12 → cw 7.200      16 → cw 9.600      20 → cw 12.000
font_size 15 → cw 9.000      17 → cw 10.200     24 → cw 14.400
```

The marker's ink left edge is `p − cw` (plus `rect.x`) from the canvas edge; the stroke's inner edge
is `inset = 4.0` px out. So it crosses the stroke when `cw > p + inset + rect.x`, i.e. at the
defaults `cw > 10.0 + rect.x`: **≥ 16 px from the best layout phase (`rect.x ≈ −0.5`, `cw 9.6`),
≥ 18 px from every phase (`rect.x = +0.5`, `cw 10.8`)**. Clearance of the marker's left ink edge from
the stroke's inner edge at `rect.x = 0` is `p + inset − cw = 10.0 − cw`:

| `font_size` | `cw` | clearance from stroke | where the ink lands |
|---|---|---|---|
| 12 (default) | 7.2 | **+2.8 px** | drawn 1.2 px left of the canvas edge, in the gutter — on screen (iced clips nothing, §3), simply not touching the stroke |
| 15 | 9.0 | +1.0 px | gutter, 1 px short of the stroke |
| 16 | 9.6 | +0.4 px (worst phase `−0.1`) | touches the stroke only at `rect.x ≈ −0.5` |
| 17 | 10.2 | **−0.2 px** | 0.2 px of `search_match` orange on the 1 px stroke |
| 18 | 10.8 | −0.8 px | covers the unfocused stroke, reaches the cell edge |
| 20 | 12.0 | −2.0 px | past the 1 px stroke and past the cell edge, into the neighbour's gutter |
| 24 | 14.4 | −4.4 px | 3.4 px into the neighbouring cell |

The threshold moves with the configured padding: `cw > padding + 4.0 + rect.x` — `padding = 4` opens it
at `font_size ≥ 13` (best phase), `padding = 2` at `font_size ≥ 10`, and `padding = 0` at
`cw > 4.0 + rect.x`, i.e. `font_size ≥ 6` from the best layout phase (`≥ 8` from the worst) — not
"at any font". A
configured font family (`ui_fonts::init`, `src/ui_fonts.rs:19-45`) changes the ratio and moves the
threshold with it; the fix in §3 does not depend on the ratio.

The marker is **one row tall and `cw/8` wide** (0.9 px wide, one cell row tall, at the defaults). So: a real, visible
overhang defect — but a per-row sliver, not the full-height line at `x≈1424–1427` in the screenshot.

### 1.3 The shared edge — where the owner's report actually is

§1.1 measures one pane's canvas against **its own** stroke. The owner's answer (§9) puts the covered
stroke at the edge two stacked panes **share**, where one pane's paint meets the *neighbour's*
stroke — a case §1.1 never writes down. With `SPACING = 0.0` (`src/panes_grid.rs:98`) the cells
touch, so for a stacked split at boundary `S` (A above, B below, `S = a.y + a.height = b.y`):

* A's stroke band is `[S − frameA, S]`, B's is `[S, S + frameB]`: the two meet exactly on `S`. That
  **layout-level** invariant is already pinned — `framed_panes_carry_their_frame_on_shared_edges`
  (`src/panes_grid.rs:831-865`) runs the stacked case through `assert_framed_shared_edge` (`:738`),
  which asserts the cells touch (`gap ≈ 0`), the strokes touch (`stroke_gap ≈ 0`) and neither cell's
  terminal area reaches under a stroke. Nothing here duplicates it; what it does not pin is what is
  **painted**.
* A's canvas bottom edge sits `metrics.padding = inset + frame = 4.0 + frameA` above `S`
  (`src/panes_grid.rs:113-119,135-146,202-221`), i.e. at `S − 4.0 − frameA`. The fill's overhang is
  ≤ 0.5 px (§1.1 — `origin_offset ∈ (−0.5, 0.5]`), so it reaches at best **`S − 3.5 − frameA`**:
  **3.5 px clear of A's own stroke band, and `3.5 + frameA` px clear of B's** (which begins at `S`;
  its far edge is a further `frameB` on, i.e. `3.5 + frameA + frameB`). At the defaults — A
  unfocused, B focused — that is 3.5 px clear of A's stroke and 4.5 px clear of B's.
* So **§1.1's fill row is unchanged at the shared edge: zero visible pixel, every configuration.**
  The distance to a stroke only *grows* there, because the neighbour's stroke is on the far side of
  a boundary the canvas is already `4.0 + frameA` away from.
* **§1.2's marker threshold is unchanged too, and the neighbour's stroke is strictly harder to
  reach than the pane's own.** The marker's ink left edge is `p − cw + rect.x` from the canvas edge.
  It touches the pane's own stroke at `cw > p + inset + rect.x = p + 4.0 + rect.x` (§1.2); reaching a
  neighbour across a shared edge means clearing the pane's own stroke band **and** the boundary,
  i.e. `cw > p + 4.0 + frameA + rect.x` — the same inequality plus `frameA > 0`. Every font that
  touches a neighbour's stroke already touches the pane's own, so §1.2's table and its
  `font_size ≥ 16…18` verdict stand at a shared edge. (The marker only leaves a canvas on its
  **left**, so this is the side-by-side case; a vertically stacked pane's left edge is a window edge
  or another split, same arithmetic.)
* With frames off there are no strokes (`frame = 0`, `inset = 0`, `metrics.padding = 0`): the fills
  meet, both are `theme.bg`, and there is nothing to cover (§1.1).

**Consequence for stage 3, stated before it starts: nothing this plan changes is at a shared
edge.** `paint_rect` moves no pixel (§3), and the marker clamp moves an x that already has
`p + inset − cw ≥ 2.8 px` of clearance from the nearest stroke at the defaults (§1.2). The paint
excursion is pinned by a new test (§4, test 3) — deliberately *not* by extending
`framed_panes_carry_their_frame_on_shared_edges`, which reasons about cell and terminal-area rects
and would stay green through a paint change that walked a fill out over a stroke.

## 2. Candidates, ranked, with the evidence that decides them

| # | Cause | Evidence | Verdict |
|---|---|---|---|
| C1 | background fill overhang from `origin_offset` (`§1.1`) | ≤ 0.5 px into a 4.0 px `theme.bg` gap, same colour | **cannot produce any visible pixel** — hygiene only |
| C2 | the terminal keeps one more row/column than fits | `cols = ⌊(w − 2p)/cw⌋` → the last cell ends ≥ 5.5 px inside | **ruled out** by arithmetic (`bounds_to_cols_rows_calculations` already covers the fit) |
| C3 | the search rows' gutter marker (`§1.2`) | `x = ox − cw`; crosses the inset at `cw > p + inset + rect.x`, measured `font_size ≥ 16…18` at defaults, `≥ 10` at `padding = 2` | **the only primitive in `paint_pane` that reaches a stroke** — real, configuration-dependent, not the screenshot |
| C4 | the reported line is not a pane frame at all but chrome/overlay (tab bar, status bar, search overlay, sessions modal), or something outside this tree | nothing in `master` places a full-height line at the image's right edge (`§0`) | **only the owner can say** — §9, asked with this card |
| C5 | **the shared edge of a stacked split** (the owner's own answer, §9): a stroke that is missing/covered *where two panes touch* | §1.3: no `paint_pane` primitive gets within 3.5 px of a shared edge at any configuration, so the paint cannot be the cause; PR #16 (`46c1ce1`) was merged against this exact symptom — "fix(panes): draw the frame on the edges two panes share (owner: frames are missing where panes touch)" | **the owner's report, and outside this diff** — first lead of its own card: an incomplete/regressed PR #16 fix, or a layer above the canvas (container `Border` with `frame_w`, `pane_grid`'s cell rects) |

## 3. Decision — design for stage 3

Two changes; the first is the defect fix, the second is hygiene that makes "a pane paints only inside
its canvas" true by construction. **Neither touches the shared-edge symptom** (§1.3, §2 C5): this
diff is the fix for a real overhang that is not the owner's report, and stage 3 must not present it
as one.

1. **Bound the search marker (the real defect, `src/pane_canvas.rs`).** The marker belongs in the
   gutter left of the text origin, but the gutter is only `p` wide, so a cell wider than the gutter
   puts it outside the canvas. Clamp it:

   ```rust
   /// The x a search row's gutter marker is drawn at: one cell left of the text
   /// origin, but never outside the canvas.
   ///
   /// The gutter is only `padding` wide, so a cell wider than the gutter would
   /// otherwise put the marker's ink past the canvas edge and onto the pane's own
   /// frame stroke. Clamping keeps the marker visible and the stroke intact.
   fn marker_x(padding: f32, cell_w: f32) -> f32 {
       (padding - cell_w).max(0.0)
   }
   ```

   used at `src/pane_canvas.rs:289` in place of `ox - cw` (`ox = rect.x + padding`, `:166`). While
   `cell_w ≤ padding` — `font_size ≤ 10` at the defaults — the value is `padding − cell_w` exactly,
   i.e. today's position; above that the marker stops at the canvas edge instead of marching toward
   the stroke (and, from `font_size 18`, into the neighbour). In between it moves right by
   `cell_w − padding`: 1.2 px at the default `font_size 12`.

2. **Keep the painted rect inside the canvas (hygiene, `src/panes_grid.rs`).** Extract the inline
   math into a documented helper

   ```rust
   /// The rect a pane paints into: the canvas' bounds, snapped to whole pixels
   /// without growing past them.
   fn paint_rect(bounds: Rectangle) -> Rectangle
   ```

   returning `Rectangle { x: fx, y: fy, width: bounds.width - fx, height: bounds.height - fy }` where
   `fx = bounds.x.round() - bounds.x`, `fy = bounds.y.round() - bounds.y`. It keeps the "rounded
   origin" intent the module was built around and stops the rect growing past the edge. **It moves no
   pixel** (§1.1) and the plan says so rather than selling it as the fix; it is what makes the
   invariant checkable in a unit test.

**Deliberately not done: `frame.with_clip(...)` around `paint_pane`.** Revision 1 proposed it and §7
claimed it changed no content; both were wrong. Measured: at the defaults the marker's ink is at
canvas-local `−1.2 … −0.3`, i.e. **entirely outside the canvas**, and it is on screen only because
iced hands the canvas no clip (`iced_widget-0.14.2/src/canvas.rs:290-312`, `_viewport` unused;
`container::clip(true)` only narrows the viewport, `container.rs:351-372`). A clip over the canvas
bounds would therefore **delete the search marker on every pane at default metrics** — a regression
traded for nothing, because the only other thing that leaves the canvas is the fill overhang, which
is invisible and already removed by change 2. `Frame::with_clip` exists
(`iced_graphics-0.14.0/src/geometry/frame.rs:145-155`) and its region is frame-local, so the
mechanism is available and correctly understood — it is simply the wrong instrument here.

## 4. Tests (assertable without a display)

* New `panes_grid::tests::paint_rect_stays_inside_the_canvas` — table of bounds: integer
  (`x: 592.0, y: 24.0, 597.0x640.0` → rect == bounds exactly), both-fractional
  (`x: 592.5, y: 24.25, 597.75x640.5`), and the worst cases `x.999`/`y.999`; assert
  `rect.x >= b.x`, `rect.y >= b.y`, `rect.x + rect.width <= b.x + b.width`,
  `rect.y + rect.height <= b.y + b.height`, `rect.x.fract() == 0.0`, `rect.y.fract() == 0.0`.
  **RED against today's code**: with `x: 592.5, width: 597.75` the shipped rect ends at `1190.75`
  while the canvas ends at `1190.25`. Run it against the current `draw()` body first (temporary
  `#[cfg(test)]` copy of the inline math, or run it before the helper exists) and paste the failure.
* New `pane_canvas::tests::search_marker_stays_inside_the_canvas` — table over `(padding, cell_w)`:
  `(6.0, 7.2)` → today `−1.2`, expected `0.0`; `(6.0, 9.6)` → `−3.6` → `0.0`; `(6.0, 10.8)` →
  `−4.8` → `0.0`; `(6.0, 20.0)` → `−14.0` → `0.0`; `(12.0, 7.2)` → `4.8` unchanged; and a sweep
  `cell_w 1.0 … 32.0` at `padding 6.0` asserting `x >= 0.0` and `x + cell_w/8.0 <= 6.0 + cell_w`
  (the marker never leaves the canvas on any font). **RED evidence**: the same table evaluated
  against today's `padding - cw` expression, pasted on the card — four negative rows.
* New `panes_grid::tests::painted_fill_cannot_reach_a_shared_edge` — the **painted** half of §1.3,
  for the owner's own configuration: a **stacked** split (`SplitDir::Vertical`, the setup the layout
  test already uses at `:845-850`), `frames = true`, both focus combinations. From `regions()`
  (`:697-703`, the engine's own layout) with `a` above `b`: `S = b.y` (assert
  `b.y − (a.y + a.height) ≈ 0`, i.e. the cells touch), `canvas_bottom = a.y + a.height − m_a.padding`,
  `fill_reach = canvas_bottom + 0.5` (§1.1's worst-case overhang). Assert
  `fill_reach <= a.y + a.height − m_a.frame − 3.5 + 0.01` (clear of A's own stroke band) **and**
  `fill_reach <= S − (3.5 + m_a.frame) + 0.01` (clear of B's); mirror both on `x` for the
  side-by-side split. **This one is GREEN on today's tree** — it pins a property the tree already
  has, so unlike the other two it is not a RED test: write it first, watch it pass, and say so on
  the card instead of dressing it up as a failure. It is a guard, not a discovery, and a real one:
  `S` comes from `pane_grid`'s own layout (`regions()`), not from this crate's formula, so the test
  fails the day `SPACING` stops being 0, a cell is handed out short/overlapping, or `cell_metrics`
  stops including the frame and the 4.0 px inset in `padding` — exactly the changes that would let a
  fill reach a stroke.
* Everything after PR #16 must stay green: `framed_panes_carry_their_frame_on_shared_edges`,
  `frameless_panes_touch_with_no_gutter`, `click_to_pane_index_mapping`,
  `stack_label_string_formatting`, `bounds_to_cols_rows_calculations`.

## 5. Verification (stage 3 runs these and pastes the real output)

Through the build gate, per-card target dir, never a bare `cargo`:

```sh
CARGO_TARGET_DIR=/var/build/ghostwave/t_b6e8cb0c \
  bash ~/.hermes/scripts/build_gate.sh \
  nix-shell --run 'cargo test --locked panes_grid::'
# expect: 7 passed / 0 failed  (5 existing + paint_rect_stays_inside_the_canvas + painted_fill_cannot_reach_a_shared_edge)

CARGO_TARGET_DIR=/var/build/ghostwave/t_b6e8cb0c \
  bash ~/.hermes/scripts/build_gate.sh \
  nix-shell --run 'cargo test --locked pane_canvas::'
# expect: 1 passed / 0 failed  (the new marker test; pane_canvas has no other unit tests)

CARGO_TARGET_DIR=/var/build/ghostwave/t_b6e8cb0c \
  bash ~/.hermes/scripts/build_gate.sh nix-shell --run 'cargo test --locked'
# expect: 0 failed / 0 ignored, and passed == whatever this same command reports on the base + 3
#   measured at the base a9c2f97, by enumeration: 259 test functions
#   (grep -rho '#\[test\]' src/ = 252, tests/ = 7; #[ignore] = 0, cfg(feature = 0, no doctests)
#   → 262 after the three new tests. Do NOT reuse 218: that is PR #16's tree (46c1ce1), two
#   merges stale (c8b7404 +24, a9c2f97 +17). Run the base count yourself and paste both numbers.

CARGO_TARGET_DIR=/var/build/ghostwave/t_b6e8cb0c \
  bash ~/.hermes/scripts/build_gate.sh nix-shell --run 'cargo clippy --locked --all-targets'
# expect: rc 0, no new warnings (any pre-existing warning in a touched file is recorded, not fixed)
```

Gate exit 3 = another build holds the lock → comment and block the card `transient`; exit 4 = not
enough RAM → retry. Never compile around the gate.

**Not verifiable here, by construction:** orac is headless (`nixos` aarch64, no display, no GPU/EGL
surface), so the *visual* result cannot be produced on this box. Stage 3 must state on the card, and
must not claim a pixel moved:

```
NEEDS-OWNER-VERIFY: at font_size 12 / padding 6 (defaults) the only intended visible change is the
search marker moving 1.2 px right, canvas-local -1.2 -> 0 (unfocused cell-local 3.8 -> 5.0,
focused 4.8 -> 6.0 - state the canvas-local number, it is the one that does not move with focus);
nothing else should look different. At font_size >= 17 a search row's orange gutter marker used to
sit on the pane's frame stroke and must now stop at the pane's edge. This diff does NOT address the
owner's report: the covered stroke is at the shared edge of a stacked split (PLAN.md 1.3 and 9),
where nothing this diff paints changes - that symptom is its own card, first lead PR #16 (46c1ce1).
```

## 6. Risks / traps

* **The owner's symptom is not in this diff at all — now confirmed, not suspected.** The owner's
  answer (§9) puts it at the shared edge of a stacked split, and §1.3 shows no `paint_pane`
  primitive reaches such an edge at any configuration. The symptom becomes its own card (§9); this
  plan does not chase it, and stage 3 must not describe its diff as closing the report.
* **The marker's clamp is a small visible move** at `cell_w > padding` (≤ 1.2 px at the defaults) —
  unavoidable, since the alternative that keeps it still is letting it paint over the stroke.
  Do not "improve" it by clipping: that deletes the marker at the defaults (§3).
* **`cw` depends on the font.** The clamp is ratio-independent, but the *threshold* at which the
  defect shows is not: 0.600 for the bundled face, different for a configured family. Do not hardcode
  0.6 anywhere in the fix or tests beyond documenting the measured number.
* **Two paddings disagree** (`config.padding` 6.0 in the painter vs `metrics.padding` 5.0/6.0 in the
  layout). It does not cross a stroke at the defaults and is out of scope (§7); do not "fix" it here
  by feeding `metrics.padding` into the painter — that would move every glyph in every pane.
* **Do not "fix" this by widening `PANE_INSET`.** The gap already exists (4 px + stroke) and PR #16's
  shared-edge invariant (`framed_panes_carry_their_frame_on_shared_edges`) would have to move with it.
* **`paint_rect` changes no pixel.** If stage 2 wants a one-change diff, dropping it leaves the
  marker fix intact; keeping it costs six lines and makes the invariant testable.

## 7. NOT doing (explicit)

* No `with_clip` around `paint_pane` or around the fill — measured to crop the search marker at the
  defaults and to fix nothing else (§3).
* No change to `SPACING`, `PANE_GAP`, `PANE_INSET`, `TITLE_H`, the stroke widths or any theme colour.
* No redesign of the grid, the title band, the frames-off path, floats, or the overlay stack.
* No patch or vendoring of iced; `Cargo.lock` untouched.
* No change to `src/pane_canvas.rs`'s painting content beyond the marker's x (§3, change 1) — cell
  colours, glyphs, cursor and the text origin stay as they are. Revision 1 claimed "containment
  only"; that was false both ways (the clip changed content, the clamp is a content change) and is
  withdrawn.
* No fix for the `config.padding` / `metrics.padding` disagreement; no scrollbar, chrome or overlay
  work; the screenshot's element is §9's.
* **No work on the shared-edge symptom** (§1.3, §2 C5) — not in this branch, not in this diff. It is
  carved out to its own card in §9, with PR #16 `46c1ce1` as its first lead.
* No merge, no push to `master`; the PR is stage 3's, and `death916/ghostwave` is public so the merge
  is the owner's.

## 8. Ordered work items for stage 3

1. In `/home/death916/code/ghostwave/.worktrees/t_b6e8cb0c` (branch `wt/t_b6e8cb0c`, already merged
   up to `master` @ `a9c2f97`), confirm `git status` is clean and `git log -1` is this plan commit.
2. **Measure the base count first**, before touching `src/`: run the full suite through the gate on
   the base commit and record `N passed / 0 failed / 0 ignored`; expect 259 (the enumeration in §5).
   That number, +3, is §5's acceptance — never a quoted constant from an older card.
3. Paste the RED evidence for the two defect tests before adding either helper: the current inline
   rect math for `paint_rect_stays_inside_the_canvas`, and `padding − cell_w` for
   `search_marker_stays_inside_the_canvas` (§4). Run `panes_grid::` through the gate, record the
   failures, then delete the temporary copies. Test 3 (`painted_fill_cannot_reach_a_shared_edge`) is
   the exception: add it first and record that it **passes on the base tree** (§4).
4. Add `fn marker_x(padding: f32, cell_w: f32) -> f32` in `src/pane_canvas.rs` with the doc comment
   and use it at `:289`; add its test.
5. Add `fn paint_rect(bounds: Rectangle) -> Rectangle` in `src/panes_grid.rs` with the doc comment and
   use it in `draw()`; add its test.
6. Run §5's four commands; paste `N passed / M failed`, clippy rc, and `rustfmt --check` on the two
   touched files (this module has pre-existing import-order hunks — say so, do not "fix" them).
7. `PROGRESS.md`: one dated entry in the repo's running-log style, recording the revision-3
   arithmetic (4 px inset, 0 visible px for the fill, 3.5 px clearance at a shared edge, the marker
   threshold, the rejected clip) and the fact that the owner's shared-edge report is **not** this
   diff's — with PR #16 named as its first lead.
8. One commit for the behaviour (`fix(panes): keep a pane's canvas inside its frame`), push the
   branch through `~/.hermes/scripts/fleet_push.sh dev "$PWD" origin wt/t_b6e8cb0c`, open the PR
   against `master` and read it back (state, head sha, base, mergeable).
9. Mark the card `NEEDS-OWNER-VERIFY` with §5's wording (headless box), repeat in the summary that
   the owner's shared-edge report is carved out (§9) and not closed by this PR, and hand stage 4 to
   `review`.

## 9. OWNER QUESTION — answered mid-review; the answer reframes the job

Revision 2 asked which border in `https://ibb.co/VWvPmKyj` is being covered, because §0 finds no
theme frame colour in the image to measure. The owner answered on the dashboard while round 2 was in
review — **verbatim**:

> "it was a 2 pane split one above the other the black canvas ( it hink) overlaps the border.
>  i cans ee the border highlight color all around the pane escept where they touch"

Read literally: a **two-pane split, one pane above the other**; the frame highlight is intact on every
side **except the shared edge**. That is exactly the configuration §1.3 computes and §0/§1.1 never
addressed.

**What the answer buys — and what it does not.** Revision 2's mapping, "'a pane frame' → stage 3's
marker clamp plus the snap is the whole fix", is **withdrawn: it is refuted by this answer and by
this plan's own arithmetic.** At a shared edge a pane's fill stops ≥ 3.5 px short of its own stroke
and ≥ 3.5 + frameA px short of the neighbour's (and 3.5 + frameA + frameB short of the neighbour's
far edge — §1.3); the marker is a one-row, `cw/8`-wide
sliver that at `font_size ≤ 15` never leaves the gutter (§1.2). **Nothing stage 3 changes is painted
at a shared edge, so stage 3 does not close the owner's report under any configuration this plan
computes.** Any wording that says otherwise is wrong and must not reach stage 3 or stage 4.

**The sharper lead:** this is the *same symptom* PR #16 was merged for. `46c1ce1` — "fix(panes): draw
the frame on the edges two panes share (owner: frames are missing where panes touch)" — and
`framed_panes_carry_their_frame_on_shared_edges` (`src/panes_grid.rs:831`) pins the layout that fix
produced. The owner reporting the shared edge again therefore points at an **incomplete or regressed
PR #16 fix, or at a layer above the canvas** — the container `Border` with `frame_w`
(`src/panes_grid.rs:242-250`), the cell rects `pane_grid` hands out, or the stroke's colour/alpha —
not at a new mystery, and certainly not at `paint_pane`.

**Carve-out — the shared-edge symptom is its own card, not this branch's.** Spec recorded here so
the card needs no re-derivation:

* **title:** `a pane frame is still covered on the edge two stacked panes share`
* **first lead:** PR #16 `46c1ce1`, `framed_panes_carry_their_frame_on_shared_edges` and
  `assert_framed_shared_edge` (`src/panes_grid.rs:738`) — note what the test asserts (layout rects
  only: cells touch, strokes touch, no terminal area under a stroke) and therefore what it cannot
  catch (anything painted), then check whether the container `Border` is drawn on the shared edge at
  all and with which colour and alpha.
* **still owed by the owner before it starts:** `font_size`, `padding` and `show_frames` from
  `~/.config/ghostwave/config.toml` (defaults 12.0 / 6.0 / on — `src/config.rs:157,161`) and an
  unscaled capture of the stacked split. The supplied image is resampled to 1492x922 and carries no
  theme frame colour at all (§0), so it cannot be measured to the pixel.
* **where it lives:** its own branch and card, raised by the orchestrator against the owner's answer.
  Not `wt/t_b6e8cb0c`, not stage 3, not this PR.

The honest statement stays, and is repeated in §5's `NEEDS-OWNER-VERIFY` and in stage 3's summary:
**stage 3 must not be sold as closing the owner's report.**

## 10. What stage 2 has to judge **revision 3** on

1. **B1:** is §5's full-suite expectation now true at the base commit — measured-then-incremented
   (+3, 262 at `a9c2f97`), with the measurement shown rather than quoted — and are the per-module
   rows (`panes_grid::` 5 → 7, `pane_canvas::` 0 → 1) still right? Is the move from +2 to +3
   justified by §4's third test rather than smuggled in?
2. **B2:** is §1.3's arithmetic right for the stacked case — canvas edge at `S − 4.0 − frameA`, fill
   reach `S − 3.5 − frameA`, clearances `3.5` / `3.5 + frameA` / `3.5 + frameA + frameB` — and is it
   right that the marker's own-stroke threshold is met strictly before the neighbour's? Is §4's test
   3 the right pin, and is the plan honest that it is **green before** the diff?
3. **B3:** does §9 carry the owner's sentence verbatim, withdraw the "the clamp is the whole fix"
   mapping, name PR #16 `46c1ce1` as the first lead, and carve the symptom into its own card with the
   owed inputs? Is there anywhere left in this plan that sells stage 3 as closing the report?
4. **M1–M4:** is the cursor row now an explicit assumption with its FFI citation and its own
   threshold (not a fake "no")? Does `padding = 0` open at `font_size ≥ 6`? Does the `font_size 12`
   row stop calling a visible gutter marker invisible? Is the `NEEDS-OWNER-VERIFY` move stated
   canvas-local (`−1.2 → 0`)?
5. **Standing:** §1.1's four rows, §1.2's table, §3's two changes, §4's RED/GREEN expectations, the
   scope in §7, and the no-clip finding that decides against `with_clip`.

---

# PLAN — libghostty-vt 0.2.2 now; ghostty 1.4's library gains are a separate card; and the 761 lines of hand-rolled selection the library already replaced

Card: `t_df08b599` · repo `/home/death916/code/ghostwave` · worktree `.worktrees/t_df08b599` · branch `wt/t_df08b599` off `master` `a91a1a8`
Owner's ask (verbatim): *"ghostty has released version 1.4 and i think libghostty was been updated. see if our depencies can be updated for it and also see if any of the featurees it brings weve already implemented and use the library ones instead. check for other things from lib we could use instead of handroll also to save on our own code complexity and ge tthe full benefits of libghostty"*

## 0. The answers, before the evidence

1. **"Can our dependencies be updated for 1.4?"** Not by bumping what we depend on — but a 1.4 binding *does* exist, and it is not the crate we use. **`ghostty-vt` / `ghostty-vt-sys` `0.8.0+ghostty.1.4.0-main.83fb956`** were published **2026-10-28T07:23Z** (§1), a fork of our own dependency ("Adapted from libghostty-vt 0.2.2", `ghostty-vt src/lib.rs:37`) re-pinned to ghostty `83fb956` = `1.4.0-main`. Adopting it is a **migration, not a bump**: it replaces our 9 `on_*` callbacks with an `Event` drain, drops allocators, and needs rustc 1.96 + Zig 0.17 (§4). The crate we actually depend on — **`libghostty-vt` 0.2.2** (2026-09-28) — vendors **the same ghostty commit as 0.2.1**: `a887df42c56f6de86c0fe6da9c4eeca37931e083`, `1.3.2-dev`, dated 2026-07-11, so *our* dependency carries no 1.4 API at all. What we do in this card is **0.2.1 → 0.2.2**, an API-identical soundness release (§2), and the fork goes to its own card (§4 Routing).
2. **"Features 1.4 brings that we already implemented — use the library ones."** None of the 1.4-only APIs is implemented by us. The two hand-rolled subsystems 1.4 *would* replace are find-in-rows (`src/search.rs`) and session replay (`src/session.rs` pane logs); both are un-swappable inside this card — our pin has no 1.4 API, and the one published 1.4 binding is the migration of §4 — so they are a follow-up card, not stage 3.
3. **"Other things from the library we could use instead of hand-rolling."** Yes, three, all available in the pin we already link: a **dead 761-line hand-rolled selection module** (`src/selection.rs`), **scrollback compression** (`Terminal::compress`, never called — and it is 1.4's headline memory feature), and **OSC 8 hyperlinks** (`GridRef::hyperlink_uri`, while we re-scrape row text in `link_at_cell`).

## 1. Measurement log (every claim below is reproducible)

Registry/API, 2026-10-28:

```sh
curl -sS -H 'User-Agent: ghostwave-dev-plan' https://crates.io/api/v1/crates/libghostty-vt
#   {"max_stable_version":"0.2.2","newest_version":"0.2.2", ... "created_at":"2026-09-28T12:46:33Z"}
curl -sS -H 'User-Agent: ghostwave-dev-plan' \
  https://crates.io/api/v1/crates/libghostty-vt-sys/0.2.2   # features: vendored (default), kitty-graphics, link-dynamic, pkg-config, bindgen-tool
```

The registry landscape, from a search (this is the command that reproduces §0.1 and §4 — the two per-crate lookups above cannot see a crate that is not `libghostty-vt`):

```sh
curl -sS -H 'User-Agent: ghostwave-dev-plan' 'https://crates.io/api/v1/crates?q=libghostty&per_page=50'
#   libghostty · libghostty-vt · libghostty-vt-sys · ghostty-vt · ghostty-vt-sys ·
#   ghostty-vt-rs-sys · mnml-libghostty-vt(-sys) · khostty-vt · spyc-vt-sys ·
#   zgx-ghostty-sys · embers-ghostty-sys · betamax-core · qwertty-term-vt · …
```

Two *projects* there are real candidates for a 1.4 binding — three crates — and neither is ours (per-crate `.../api/v1/crates/<name>`, same headers, 2026-10-28T07:2xZ):

| crate | version | published | `rust-version` | downloads (crate / that version) |
|---|---|---|---|---|
| `ghostty-vt` | `0.8.0+ghostty.1.4.0-main.83fb956` | 2026-10-28T07:23:15Z | 1.96 | 362 / 15 |
| `ghostty-vt-sys` | `0.8.0+ghostty.1.4.0-main.83fb956` | 2026-10-28T07:23:13Z | 1.96 | 459 / 21 |
| `ghostty-vt-rs-sys` | `0.2.0` | 2026-10-27T18:59:19Z | 1.90 | 565 / 50 |
| `libghostty-vt` (ours) | `0.2.2` | 2026-09-28T12:46:33Z | 1.90 | 182 706 / 8 485 |

`ghostty-vt` is the only **safe** 1.4 binding, and it is what §4 evaluates. Measured from the downloaded tarball:

```sh
# ghostty-vt-0.8.0+ghostty.1.4.0-main.83fb956
grep -n 'Adapted from' src/lib.rs                      # 37://! Adapted from libghostty-vt 0.2.2 (MIT OR Apache-2.0).
grep -n 'rust-version' Cargo.toml                      # 14:rust-version = "1.96"
# ghostty-vt-sys-0.8.0+…/GHOSTTY.lock:
#   commit = 83fb95624b0439ee660c2679b43b38c8b63769cd
#   version = 1.4.0-main+83fb956
grep -n 'pub fn ' ghostty-vt-…/src/search.rs           # Search::new/tick/run/feed/matches/viewport_matches/select_next/select_prev/…
grep -n 'pub fn \|pub struct ' ghostty-vt-…/src/snapshot.rs   # encode, encode_to, Decoder::{new,decode,ready,next_page}
for s in ghostty_search_new ghostty_snapshot_encode ghostty_terminal_paste \
         ghostty_terminal_continuation_alloc ghostty_terminal_mode_get; do
  printf '%-40s %s\n' "$s" "$(grep -c "$s" ghostty-vt-sys-…/src/bindings.rs)"; done
#   ghostty_search_new 1 · ghostty_snapshot_encode 3 · ghostty_terminal_paste 6
#   ghostty_terminal_continuation_alloc 1 · ghostty_terminal_mode_get 0
```

So the 1.4 API surface §3 needs **is published and wrapped** — by a fork, not by our crate. `ghostty_terminal_mode_get` is gone, but the safe crate keeps `Terminal::mode()` (`src/terminal.rs:409`, now returning `Result`), so the loss is at the C level, not at the API level we use (`term.rs:840`).

`ghostty-vt-rs-sys` 0.2.0 is **raw FFI only** — there is no `ghostty-vt-rs` safe crate on crates.io (`GET /api/v1/crates/ghostty-vt-rs` → 404) — but it is the interesting second lead: it ships **prebuilt static libraries** for seven targets including `aarch64-unknown-linux-gnu` (`prebuilt.rs:9`), so it needs **no Zig at all**, and its pinned commit `4a4fc017…` is dated 2026-10-27T16:34Z with the message `up to 1.4.1-dev`. It carries the same 1.4 symbol set (`ghostty_search_new`, `ghostty_snapshot_encode`, no `ghostty_terminal_mode_get`). Writing our own safe wrappers over it is strictly more work than §4's migration, so it is recorded as an alternative, not a route.


Crate source diff, `0.2.1` vs `0.2.2` (both tarballs downloaded and unpacked side by side):

```sh
diff -ru src-0.2.1 src-0.2.2     # 237 diff lines, exactly 4 touched files
```

| file | change |
|---|---|
| `Cargo.toml`, `Cargo.lock`, `.cargo_vcs_info.json` | version bump, `libghostty-vt-sys` 0.2.2 |
| `src/terminal.rs` | the only source change: clipboard soundness (below) |

**No module added, no public signature changed, no new safe wrapper.** The 0.2.2 change is upstream issue #74: `ClipboardWrite::contents()` no longer calls `slice::from_raw_parts` on `NULL` (the "clear clipboard" shape: OSC 52 with an empty payload), and `ClipboardContent { mime, data }` no longer turns non-UTF-8 bytes into an invalid `&str` (`data` degrades to `""`; `mime` falls back to `application/octet-stream`). Both hunks are in `src/terminal.rs` at the `contents()`/`ClipboardContent::from_raw` sites; the tests added are `#[cfg(miri)]`.

Vendored engine, both versions:

```sh
grep -n GHOSTTY_COMMIT libghostty-vt-sys-0.2.{1,2}/build.rs
#   const GHOSTTY_COMMIT: &str = "a887df42c56f6de86c0fe6da9c4eeca37931e083";   ← identical, both
diff -u libghostty-vt-sys-0.2.1/src/bindings.rs libghostty-vt-sys-0.2.2/src/bindings.rs
#   (empty) — the FFI surface is byte-identical
git -C <ghostty checkout> log -1   # a887df4 2026-07-11, build.zig.zon: .version = "1.3.2-dev"
```

Ghostty 1.4.0 was released **2026-10-27** (ghostty.org release notes), i.e. one day before this card, and three weeks *after* the newest `libghostty-vt` crate. It requires **Zig 0.17** to build ("Changes for Package Maintainers"); our `shell.nix` pins `zig_0_15` (0.15.2, `shell.nix:27`) with the comment that the pinned ghostty needs it (`shell.nix:8-9`).

Toolchain on this box, measured (every number is a command in §4.2):

```sh
nix-shell ./shell.nix --run 'rustc --version; zig version'
#   rustc 1.95.0 (59807616e 2026-04-14) · 0.15.2   (/nix/store/d01f6ax…-zig-0.15.2/bin/zig)
nix-instantiate --eval -E '(import <nixpkgs> {}).lib.version'          # "26.05pre-git"
nix-instantiate --eval -E '(import <nixpkgs> {}).zig_0_17.version'
#   error: undefined variable — "Did you mean one of zig_0_12 … zig_0_16?"   ← no 0.17 in nixpkgs
nix-instantiate --eval -E '(import <nixpkgs> {}).rustPackages_1_97.rustc.version'   # "1.97.1"
nix-instantiate --eval -E '(import <nixpkgs> {}).rustPackages_1_98.rustc.version'   # "1.98.1"
curl -sS https://ziglang.org/download/index.json | grep -c '0.17.0'    # 0.17.0, 2026-10-01
```

C API delta, our pin vs `v1.4.0` (headers fetched from the tag, symbol sets diffed):

```sh
grep -rho 'ghostty_[a-z_0-9]*' <pin>/include/ghostty/vt/ <pin>/include/ghostty/vt.h | sort -u   # 190
grep -rho 'ghostty_[a-z_0-9]*' h14/include/ghostty/vt/ h14/include/ghostty/vt.h | sort -u      # 215
```

New in 1.4 (38 symbols): `ghostty_search_{new,set,get,get_multi,run,feed,tick,free}`, `ghostty_snapshot_{encode,encode_alloc,encode_buf}`, `ghostty_snapshot_decoder_{new,new_buf,decode,next,ready,set,get,get_multi,free}`, `ghostty_terminal_continuation_{alloc,buf,write}` + `ghostty_terminal_vt_write_until`, `ghostty_terminal_paste`, `ghostty_kitty_graphics_animation_tick`, `ghostty_render_state_image_{get,get_multi,iterator_new,iterator_next,iterator_free}`, `ghostty_render_state_{clean,row_iterator_next_dirty}`, `ghostty_formatter_format`, `ghostty_osc_set`, `ghostty_wasm_*` rework.
Removed/renamed in 1.4 (breaking): `ghostty_terminal_mode_get/set` → the `GHOSTTY_TERMINAL_OPT_MODE` terminal option (`ghostty_terminal_set(term, GHOSTTY_TERMINAL_OPT_MODE, &cfg)`, `terminal.h:2050`), `ghostty_render_state_colors_get`, `ghostty_vt_osc_free` → `ghostty_osc_free`, and the per-type `ghostty_wasm_alloc_*` helpers collapse to `ghostty_wasm_alloc/free`.

## 2. Why 0.2.2 is not cosmetic for us

Our clipboard path takes the shape the fix is about. `src/term.rs:349`:

```rust
.on_clipboard_write(|_term, clip| {
    for content in clip.contents() { ... }
```

Any program can send OSC 52 with an empty payload ("clear the clipboard"); libghostty then passes `contents = NULL, contents_len = 0`, and 0.2.1's `contents()` runs `std::slice::from_raw_parts(null, 0)` — UB that the rustbindings' own regression test documents (upstream #74, "`from_raw_parts` requires a non-null pointer even at length zero"). That is reachable from ordinary terminal content, so the bump is a soundness fix on a live code path, not housekeeping.

**Honesty constraint for stage 3:** no test we can write *proves* the UB is gone — the upstream tests are Miri-gated and Miri is not part of our toolchain. Stage 3 must say exactly that: the evidence is (a) the version bump, (b) the upstream diff, (c) a regression test that the empty-payload OSC 52 shape is handled without a panic and without a clipboard write, which pins the behaviour but cannot demonstrate the absence of UB.

## 3. What 1.4 would give us, and what it would replace here

| 1.4 C API | what we have today | size it would delete |
|---|---|---|
| `ghostty_search_*` (`search.h`) | `src/search.rs` — re-scans every row through `RowIteration` on each tick (`run_on_rows`), plus the match model in `src/search_overlay.rs` | 322 lines + the match/highlight bookkeeping; lib matches are `Selection`s that survive alt-screen switches, resize/reflow and scrollback pruning — ours do not |
| `ghostty_snapshot_encode/decode` (`snapshot.h`) | `src/session.rs` — tee raw pty bytes to a per-pane log, front-truncate at `max_log_mb`, replay with `vt_write` on restore | the log/truncate/replay machinery (`AsyncPaneLog`, `truncate_log_front`, `replay_pane_log`) |
| `ghostty_terminal_paste` | `TermPane::paste_text` (`src/term.rs:836`) already uses `paste::encode` + `mode(BRACKETED_PASTE)` | nothing — the lib call only folds the mode check in; low value |
| kitty graphics animation / relative placements + `render_state_image_*` | we render **no** kitty images at all; we only install a PNG decoder so the core accepts them (`term.rs:1138`) | n/a — a feature we do not have |
| OSC 7501 program-status, OSC 99 notifications, OSC 72 drag&drop parsing | none | n/a — new capabilities |
| `terminal_continuation_*` | we `vt_write` whole reads | n/a — we do not chunk |

## 4. Why none of §3 can be in stage 3

Three independent blockers, each measured:

1. **Our dependency carries no 1.4 API, and the one published 1.4 binding is a migration, not a bump.** `libghostty-vt` 0.2.2 vendors ghostty `a887df42` (`1.3.2-dev`); `search`/`snapshot`/`image` wrappers do not exist in it (§1). The alternative — `ghostty-vt` `0.8.0+ghostty.1.4.0-main.83fb956` (§1) — does wrap them, and this is what adopting it costs in *our* tree, measured:
   - terminal effects become an **`Event` drain** (`Terminal::events`, `ghostty-vt src/terminal.rs:271`; `Event` enum at `:1223`, `ClipboardWrite` included) where we register **9 callbacks** — `on_pty_write`, `on_size`, `on_device_attributes`, `on_xtversion`, `on_color_scheme`, `on_title_changed`, `on_pwd_changed`, `on_bell`, `on_clipboard_write` at `term.rs:290,299,311,329,330,331,337,343,349`;
   - it has **no custom allocators and no `'alloc` lifetime**, so `format_selection_alloc(None, opts)` (`term.rs:698`, `:825`) becomes `fmt::Formatter` + `FormatterOptions::with_selection` (`ghostty-vt src/fmt.rs:20,71`), and `Bytes::new_with_alloc` (`term.rs:1163`) goes away;
   - every getter returns `Result` (its `README.md`), and the crate is Apache-2.0-only where ours is `MIT OR Apache-2.0`.
   That is **12 call sites rewritten in `src/term.rs`** — the same file stage 3 edits for the 0.2.2 bump. Doing both in one card would bury an API-identical soundness fix inside a pane/effects rewrite and make stage 4 review nothing useful.
2. **Toolchain: real, but a Nix rebuild is *not* what unblocks it.** Measured in §1, argued here:
   - **Zig** — `ghostty-vt-sys` 0.8.0 hard-checks `0.17.x` (`build.rs:437`) but *resolves* zig as `ZIG` first (`build.rs:397`), then `PATH` if it reports 0.17 (`:404`), then `mise which zig` (`:409`), panicking with "set ZIG to the Zig 0.17 binary" (`:424-429`) otherwise. So **`ZIG=/path/to/zig-0.17.0/zig` is sufficient and no `shell.nix` edit is required to point at a 0.17 binary.** Our pinned nixpkgs *has no* `zig_0_17` (highest is `zig_0_16` = 0.16.0) and `/nix/store` holds only `zig-0.15.2`, so the 0.17 binary has to come from ziglang.org — `0.17.0`, 2026-10-01, `zig-aarch64-linux-0.17.0.tar.xz` exists — or from `mise`. **A Nix rebuild cannot produce one from this channel.**
   - **rustc** — `ghostty-vt` needs `>= 1.96` (`Cargo.toml:14`); the dev shell measures **1.95.0**. nixpkgs carries `rustPackages_1_97.rustc` = 1.97.1 and `rustPackages_1_98.rustc` = 1.98.1, usable ad hoc (`nix-shell -p …`), again with no rebuild.
   - **Opposite toolchains** — these conflict *only while both sys crates are in one build graph*: `libghostty-vt-sys` 0.2.2 builds ghostty 1.3.2-dev, which requires zig **0.15.2** (`shell.nix:8-9`, `:27`), while `ghostty-vt-sys` 0.8.0 requires **0.17.x**. A straight swap has one sys crate and therefore one toolchain; a side-by-side evaluation cannot have both.
3. **Moving the pin is a source-level decision.** `libghostty-vt-sys` can be pointed at a checkout with `GHOSTTY_SOURCE_DIR`, but then the checked-in `bindings.rs` must match that checkout (it is generated, not built), and every removed symbol (§1) breaks the safe crate. That is a dedicated branch with its own review — which is exactly what Routing below is.

**Routing:** the 1.4 work is a *separate card*, title **`libghostty 1.4: evaluate ghostty-vt 0.8.0 (the published 1.4 binding) against our src/search.rs and src/session.rs`**. Its blocker is **not** "owner builds a devenv with Zig 0.17" — nothing external blocks it: `ZIG=` covers Zig 0.17 and `nix-shell -p` covers rustc 1.96+, both without a Nix edit. It is a *scoped migration* with three measured costs to weigh (12 call sites, day-old fork with 15 downloads of that version, Apache-2.0-only vendor) against the measured prize (the `search`/`snapshot` wrappers that would delete `src/search.rs` and the pane-log replay), and the second lead `ghostty-vt-rs-sys` 0.2.0 (prebuilt static libs for `aarch64-unknown-linux-gnu`, no Zig at all) is the fallback if the 0.17 toolchain proves painful. It is not carved out of this plan as stage 3 scope, and this plan does not schedule it.

## 5. What we can adopt today, measured against the code

**a. Dead hand-rolled selection — delete `src/selection.rs` (761 lines, 10 `#[test]`s).**
Every symbol in the file (`SelectionModel`, `GridCell`, `SelectionMode`, `expand_word_range`, `extract_selection_*`, `pixel_to_cell`, …) is unreferenced outside the file:

```sh
grep -rn 'SelectionModel\|GridCell\|SelectionMode\|pixel_to_cell\|cell_to_pixel' src/ tests/ | grep -v '^src/selection.rs'
#   (no output)
grep -n 'mod selection' src/main.rs          # 34:pub mod selection;   — the only reference
```

The live path already **is** the library: `src/term.rs:1005` `SelectionState` drives `selection::gesture::{Gesture, PressEvent, DragEvent, ReleaseEvent}`, `Selection::adjust`, `Terminal::set_selection`, and `copy_selection` copies through `FormatOptions::new().with_trim(true).with_unwrap(true)` (`term.rs:823`). Its four clipboard wrappers (`selection.rs:484-500`) just call `ghostwave::clipboard::{write_text,write_primary_text,read_text,read_primary_text}`, which `term.rs` calls directly. The 10 tests in the file test the dead model, so they leave with it.

**b. Scrollback compression — `Terminal::compress`, present since before 0.2.1, never called.**

```sh
grep -rn 'compress\|CompressionMode' src/       # (no output)
grep -n 'pub fn compress' <registry>/libghostty-vt-0.2.1/src/terminal.rs   # 482
```

`CompressionMode::{Incremental,Full}` + `CompressionResult::{Unsupported,Pending,Complete}` + `compression_activity()` are all in the API we already link (the 0.2.1→0.2.2 diff does not touch them). This is 1.4's memory headline ("70% to 90% less physical memory for typical text") available to us **today**, and it is caller-driven: incremental steps on idle, restart when `compression_activity()` changes. It changes storage only — "never its logical contents or scrollback limit; accessing compressed history restores it transparently".

**c. OSC 8 hyperlinks — `GridRef::hyperlink_uri` (in 0.2.1's `screen.rs:116`; the 0.2.2 diff leaves `screen.rs` untouched).**
`TermPane::link_at_cell` (`term.rs:888`) reconstructs the row as a string, strips variation selectors, and scans for `http(s)://` / path-shaped tokens. Consequence: an OSC 8 link whose display text is not its URL (the common case — `ls --hyperlink`, `git`'s docs links, systemd, many CLIs) is not clickable, and a long URI is truncated by the row scan. Fix: ask the cell for `hyperlink_uri` first (authoritative, and it survives wide chars / grapheme clusters), fall back to the text scan for bare URLs so the existing clickable-URL feature keeps working.

**d. `png` 0.17 vs 0.18 — one dependency too many.**
`Cargo.toml:30` says `png = "0.17"` for our kitty `DecodePng` impl; `libghostty-vt`'s `png` feature already pulls `png 0.18.1` (`Cargo.lock` contains both). The crate's own `RustPngDecoder` is still unusable — `graphics.rs:904` is a public struct with a private `buf` field and **no constructor** — so our impl stays, on 0.18. API delta measured against the vendored `png-0.18.1` source: identical except `output_buffer_size()` now returns `Option<usize>` (`decoder/mod.rs:668`), so the call at `term.rs:1160` needs one `?`/`unwrap_or(0)`.

**e. Considered and deliberately not in stage 3**

| candidate | why not |
|---|---|
| `paste::is_safe` | available, unused; only meaningful with an "unsafe paste?" confirm prompt — UI scope, deserves its own card |
| `focus::Event::encode` (mode 1004 in/out reporting) | we implement no focus reporting at all; adding it changes what every program receives — separate card |
| `log::set_logger` | debugging aid, not code reduction |
| `osc::Parser` for `src/font_query.rs` | the pin's `GHOSTTY_OSC_COMMAND_*` enum has **no OSC 50** (`osc.h:43-66`), so the hand-written scanner in that 339-line module is the only way to answer font queries — keep it |
| `sgr::Parser`, `unicode::grapheme_width` | nothing in our tree hand-parses SGR or computes widths; no saving |
| `fmt::Formatter` | we already copy through the lib's `FormatOptions`; the full-VT dump has no caller |
| a pty layer | libghostty-vt has none by design; `src/pty.rs` (forkpty via `nix`) stays ours |

## 6. Scope of stage 3 — the smallest honest set

**In:**
1. `libghostty-vt` `0.2.1` → `0.2.2` (`Cargo.toml:18`, lock refreshed for the crate + `libghostty-vt-sys`).
2. Unify `png` on 0.18 (`Cargo.toml:30`, one line in `term.rs`'s decoder).
3. Delete `src/selection.rs` and `src/main.rs:34` (`pub mod selection;`) — 761 lines, 10 tests.
4. Idle scrollback compression: `TermPane::compress_idle_step()` + one bounded step per pane from a dedicated low-rate ticker (§8.2 — **not** `AppState::tick`, which in the `Off` config never runs at all).
5. OSC 8 first in `link_at_cell`, text-scan fallback preserved.
6. Six new tests (§7).

**Out, with reasons recorded:** everything in §3/§4 (needs the §4 migration — a fork of our own crate, rustc 1.96, Zig 0.17 outside nixpkgs — not a dependency bump), §5e (each has its own reason), and any UI change beyond what 4 and 5 require.

## 7. Verification — commands and expected results

Baseline is measured, not quoted. On `a91a1a8`, in the dev shell, with this card's own target dir:

```sh
cd /home/death916/code/ghostwave/.worktrees/t_df08b599
export CARGO_TARGET_DIR=/var/build/ghostwave/t_df08b599
BUILD_GATE_WAIT=900 bash ~/.hermes/scripts/build_gate.sh \
  nix-shell ./shell.nix --run 'cargo test --locked'
```

> **Baseline, measured on the merged base `75da9ce` (`origin/master` brought forward on 2026-10-28, exit 0):**
>
> | target | result |
> |---|---|
> | `unittests src/lib.rs` (`ghostwave` lib) | **76 passed, 0 failed** |
> | `unittests src/main.rs` (`ghostwave` bin) | **191 passed, 0 failed** |
> | `tests/flood_throughput.rs` | 1 passed |
> | `tests/term_core.rs` | 6 passed |
> | **total** | **274 passed, 0 failed**, exit 0 |
>
> Pre-existing warnings at the base: 3, all in the bin test target (`src/main.rs:376` unused `VecDeque`, two "does not need to be mutable"); the lib target is warning-free. Stage 3 records these, fixes none of them, and adds none.
>
> (Measured twice: `273` on the pre-rebase `a91a1a8`, `274` after `origin/master` added one `src/remote.rs` test. Stage 3 starts from the merged base, so **274** is the number it must hold to.)

The 10 tests that leave with `src/selection.rs` are in the **bin** target (`selection::tests::*`, seen in the bin run above), and the six new tests land in `src/term.rs`, i.e. the **lib** target.


Per-module after the change (same gate, same target dir):

```sh
bash ~/.hermes/scripts/build_gate.sh nix-shell ./shell.nix --run 'cargo test --locked term::'
bash ~/.hermes/scripts/build_gate.sh nix-shell ./shell.nix --run 'cargo test --locked app::'
bash ~/.hermes/scripts/build_gate.sh nix-shell ./shell.nix --run 'cargo test --locked'
bash ~/.hermes/scripts/build_gate.sh nix-shell ./shell.nix --run 'cargo clippy --locked --all-targets'
```

Expected after the change: **lib 82** (76 + the six new `term::` tests), **bin 181** (191 − the ten `selection::tests::`), integration **7** unchanged, i.e. **270 passed / 0 failed** total and exit 0. These are the arithmetic of measured numbers — stage 3 still asserts them by running, and if a target disagrees it reports the real line rather than adjusting it. Clippy: no new warnings (the base's three bin-target warnings are recorded, not fixed).

Dependency proof:

```sh
cargo tree -i png --locked        # exactly one png, 0.18.x
cargo tree -i libghostty-vt --locked   # 0.2.2; sys 0.2.2
```

Deletion proof:

```sh
git grep -n 'mod selection' -- src/ tests/    # no match after the change
test ! -e src/selection.rs && echo gone
```

The six tests (all in `src/term.rs`'s `#[cfg(test)] mod tests`, no display, no pty):

1. `paste_of_empty_osc52_payload_does_not_panic` — `on_clipboard_write` installed with a `Vec` sink; `vt_write(b"\x1b]52;c;\x07")`; asserts no panic **and** no write recorded. This is the 0.2.2 shape (§2) — a behaviour pin, not a UB proof; the summary must say so.
2. `osc52_text_write_still_reaches_the_sink` — a normal `\x1b]52;c;` + base64 + BEL still writes text (no regression in the callback path).
3. `compression_returns_complete_and_keeps_scrollback` — fill 2× scrollback, then `compress(Full)`; assert `CompressionResult::Complete` (fails loudly if the target reports `Unsupported`), then assert the oldest and newest lines are still readable through the render iterator. **This is the test that proves compression actually works on this build rather than being silently unsupported.**
4. `compression_activity_is_a_token` — `compression_activity()` changes after new output and is stable when idle (the scheduling contract §5b relies on). The token is documented as **opaque, comparable only by equality, and may wrap** (`libghostty-vt-0.2.2/src/terminal.rs:493-496`: "The value may wrap and changes in either direction have the same meaning"), so the assertion is `token != previous`, never an ordering — and fill scrollback first (write enough lines to leave the primary screen) so the activity is guaranteed to move rather than depending on where a short write lands in the compression window.
5. `link_at_cell_prefers_the_osc8_uri` — write a row whose display text is `docs` and whose OSC 8 target is `https://example.com/x`; `link_at_cell(row, col_of_docs)` returns the URI, not `docs`.
6. `link_at_cell_still_finds_a_bare_url` — a plain `https://example.com/plain` row still resolves (the fallback).

## 8. Risks and how stage 3 handles them

1. **Compression must be serialized with every other terminal operation** (lib doc: "not thread-safe with other operations on the same terminal… serialize it with writes, rendering, searches", `libghostty-vt-0.2.2/src/terminal.rs:479-481`). The pane handle is `SharedPane = Arc<Mutex<Pane>>` (`src/pane.rs:26`), reached as `PaneEntry::Term(SharedPane)` (`pane.rs:602-604`) out of `Tab.panes: Vec<Option<PaneEntry>>` (`src/app.rs:86`) — there is no `Arc<Mutex<PaneEntry>>` in the tree. The stepper takes one pane's `Mutex<Pane>` and holds it across exactly one `compress` call, then releases it before touching the next pane: never two locks at once, never a lock held while sending messages.
2. **`AppState::tick` runs at 16 ms (Fast) / 125 ms (Slow) — and in `Off` it does not run at all, so compression must not hang off `tick_rate()`.** `tick_rate()` (`app.rs:643-658`) returns `Off` when there is no toast, no wave energy, `visual_wave == false`, no visualizer, `dynamic_title == Off`, no AI reply pending and no remote activity; `visual_wave` defaults to `true` (`config.rs:167`). Critically, `main.rs:658-661` does `match self.state.tick_rate() { app::TickRate::Off => {} … }` — **no ticker subscription is created**, so `AppState::tick` is never called and `TickSlot::period()`'s `Off → 3600 s` (`tick.rs:26`) is unreachable from the subscription path. A user with `visual_wave = false` and `dynamic_title = "off"` would therefore get **no compression at all** (not one step an hour) in exactly the idle-terminal scenario the feature exists for. **Decision — chosen trigger:** compression runs off a dedicated low-rate ticker, not the animation tick. Concretely: a `Message::CompressTick` delivered by a 4 s stream (modelled on `tick::stream` / `autosave::stream`) subscribed whenever any tab holds a `PaneEntry::Term`, *independent of `tick_rate()`*; its handler `AppState::compress_idle_step()` walks the panes and, per pane, reads `compression_activity()`, keeps the token cached, and calls `compress(Incremental)` **exactly once** when the token is unchanged from the cached one and the pane has been idle for ≥ 2 s since the token last changed. This is the crate's own contract ("cache it and restart its compression idle delay whenever the value changes", `terminal.rs:493-496`), it costs one token read per pane per 4 s, and it is what makes the `Off` config work. When a step returns `Complete` for the current token, nothing more is done until the token changes again. Explicitly rejected: promoting `tick_rate()` to `Slow` while work is pending (that subscribes the app's 125 ms tick-and-repaint loop — wave, toasts, visualizer — for housekeeping) and changing `tick.rs:26`'s `Off → 3600 s` (recorded here as dead code on the `main.rs:660` path so stage 3 does not "fix" it or depend on it).
3. **`CompressionResult::Unsupported` is a legitimate answer** on some targets; the code must treat it as "do nothing, remember it, stop asking", and test 3 must fail rather than skip if it appears here (x86_64/aarch64 Linux are supported per the release notes — so on orac it must be `Complete`/`Pending`).
4. **Deleting `src/selection.rs`** is safe today (grep-verified, §5a) but it is a removal of a public `mod` from the binary; a stale local branch could conflict. Stage 3 does the deletion in its own commit so it can be reverted alone.
5. **`png` 0.18 rename surface** is limited to `output_buffer_size() -> Option<usize>`; if `cargo build` disagrees with that claim the honest move is to keep 0.17 and record it, not to rewrite the decoder.
6. **`clip.at_cell` reordering (OSC 8 first)** must not change click-through for text that merely *contains* a URL with a hyperlink attached elsewhere; test 6 is the guard.

## 9. Work items for stage 3, in order

| # | change | files |
|---|---|---|
| C1 | bump `libghostty-vt` to 0.2.2, refresh lock | `Cargo.toml:18`, `Cargo.lock` |
| C2 | unify `png` on 0.18 (+ `output_buffer_size` Option) | `Cargo.toml:30`, `src/term.rs:1145-1170` |
| C3 | delete the dead selection module | `src/selection.rs` (−761), `src/main.rs:34` |
| C4 | `TermPane::compress_idle_step()` + `AppState::compress_idle_step()` + `Message::CompressTick` on its own 4 s stream (§8.2 — independent of `tick_rate()`) | `src/term.rs`, `src/app.rs`, `src/main.rs` (`Message`, update handler, `subscription()`) |
| C5 | OSC 8 first in `link_at_cell` | `src/term.rs:888` |
| C6 | the six tests of §7 | `src/term.rs` `#[cfg(test)]` |

Each item is its own commit, so stage 4 can review and revert them independently.

## 10. What stage 2 has to judge

1. **B1** Is §1's central claim right — that the crate *we* depend on cannot carry 1.4 because its `build.rs` pins `a887df42…` and its `bindings.rs` is byte-identical 0.2.1→0.2.2 — and does §1/§4 keep that separate from the **fork** that does carry 1.4 (`ghostty-vt` 0.8.0), each with a command that reproduces it rather than an assertion?
2. **B2** Is §4's cost list complete for *this* repo — the fork's 12 call sites (9 callbacks, 2 `format_selection_alloc`, 1 `Bytes::new_with_alloc`), Zig 0.17 vs a nixpkgs that has no `zig_0_17`, rustc 1.95 vs the fork's 1.96 floor, and the generated `bindings.rs`? §4 now says a Nix rebuild is **not** the blocker (`ZIG=`, `nix-shell -p rustPackages_1_97`) — is that right, and does the Routing card overstate what is already possible?
3. **B3** Is §2's reachability claim right — does `on_clipboard_write` really take the empty-payload shape — and does the plan refuse to claim a test proves the UB is gone?
4. **B4** Is §5a's "dead" claim true under the whole tree (`src/`, `tests/`, and the `ghostwave` lib target), and is the 10-test subtraction the only test-count effect?
5. **B5** Is §8.1–8.3's compression design (one pane lock at a time, one `Incremental` step per pane per 4 s ticker, idle-delay gated on `compression_activity()`, `Unsupported` is terminal-but-not-a-skip) sufficient, and is test 3 the right pin?
6. **B6** Is the scope in §6 actually the smallest honest set for the owner's ask — i.e. does it leave out something the ask plainly wanted, or include something it did not?

### Round-2 changelog (what this revision changed, and why)

| defect | what changed | evidence now in the plan |
|---|---|---|
| **D1** "only published binding" was false | §0.1, §0.2, §1 (registry landscape block + candidate table + tarball measurements), §4.1, §4 Routing | `curl …'?q=libghostty&per_page=50'`; `crates.io` per-crate versions/dates/`rust-version`/downloads; `ghostty-vt` `src/lib.rs:37`, `Cargo.toml:14`, `src/search.rs`, `src/snapshot.rs`; `ghostty-vt-sys` `GHOSTTY.lock` and `src/bindings.rs` symbol counts |
| **D2** follow-up card's blocker misstated | §1 (toolchain block), §4.2, §4 Routing (new title + no-Nix blocker) | `build.rs:397/404/409/437` (Zig resolution via `ZIG`), `shell.nix:8-9,27`, dev-shell `rustc 1.95.0 / zig 0.15.2`, nixpkgs has **no** `zig_0_17` (max `zig_0_16`), `rustPackages_1_97.rustc` = 1.97.1, ziglang.org `0.17.0` aarch64-linux tarball |
| **D3** tick-`Off` had no stated behaviour | §6 item 4, §8.2 (new decision paragraph), §9-C4, §10-B5 | `app.rs:643-658`, `config.rs:167`, `main.rs:658-661`, `tick.rs:26` |
| note: wrong pane type | §8.1, §9-C4 | `pane.rs:26`, `pane.rs:602-604`, `app.rs:86` |
| note: test 4 could be flaky | §7 test 4 | `libghostty-vt-0.2.2/src/terminal.rs:493-496` (token is opaque and may wrap; fill scrollback first) |
| note: `term.rs:1156` | §5d | the call is at `term.rs:1160` |

**One correction to the D3 brief itself.** The card says the `Off` config makes compression "advance ~once/hour". Measured, it is worse: `main.rs:658-661` never subscribes a ticker when `tick_rate()` is `Off`, so `AppState::tick` is not called at all and `tick.rs:26`'s `Off → 3600 s` is unreachable from that path. The `Off` config gets **no** compression, not a slow one — which is why §8.2 now takes the trigger off `tick_rate()` entirely instead of budgeting within the tick.

---

# PLAN — a killed durable session must not come back (`t_6abf6154`)

Card: `t_6abf6154` · repo `/home/death916/code/ghostwave` · branch `wt/t_6abf6154` off `master` `a91a1a8`.
Base suite, measured on this tree: **273** test functions (`grep -rh '^\s*#\[test\]' src/ | wc -l` = 266,
`tests/` = 7, `#[ignore]` = 0). The `ghostd` crate has **0** tests.
This is a stage-1 card: the only file it changes is `PLAN.md`.

## 0. The report, and how it was reproduced

> "starting a new durable session to a host that had a previously killed one still tries to re attacth
> and replay old session. kill remote session doesnt work properly and spawning a new one jsut goes to
> the old stuck one"

All three sentences reproduce **locally, with no ssh and no remote host**: the shipped static daemon
(`assets/ghostd-aarch64`, `ghostd 0.1.3`) driven directly, with `GHOSTWAVE_STATE_DIR` pointed at a
throwaway directory. The scripts are `repro2.sh`, `repro3.sh`, `repro5.sh`, `repro6.sh`, `repro8.sh` in
`~/.hermes/profiles/dev/cache/scratch/ghostd-repro/` — evidence for this plan, not part of the diff.

The shipped asset **is** built from this tree (`grep -c -a "wedged daemon" assets/ghostd-aarch64` = 1,
same for `busy: another client is attached`), so what is measured here is the code in `ghostd/src`.

Seven findings. Each is a mechanism read out of the code *and* an output block from a run.

### F1 — a new session replays the dead session's output

`main.rs:52` opens the log `create(true).append(true)`; `main.rs:205-219` replays the whole file to every
new client; nothing truncates it on `new`, and `cmd_kill` (`main.rs:525-564`) never removes it.

```
=== 1. fresh session s1, one command, then detach
--- log size: 370        --- marker in the log? 2
=== 2. ghostd kill s1  → ghostd: killed 's1'
--- state dir: s1.log                     ← the log outlives the daemon
=== 3. ghostd new s1 (a NEW session) then attach
--- bytes the brand-new session showed its first client: 528
--- does it contain the killed session's marker? 2
```

### F2 — `recover_session` never kills the wedged daemon

`lib.rs:112-127`: `read_to_string(pid_file)` then `pid_str.trim().parse::<i32>()`. The pidfile has been
**two lines** since the child pid was added (`main.rs:48-51`), so `trim()` yields `"2182633\n2182635"`,
the parse always fails, and the whole body is skipped — no SIGTERM, no SIGKILL. All it does is unlink the
socket and pidfile, i.e. it forgets a live daemon instead of killing it.

```
=== D3.2 new s2 over the wedged daemon
[ghostd:probe] s2: no response in 2s -> Unresponsive
[ghostd:new] s2: wedged daemon — recovering
--- old daemon after the recovery: alive          ← it was never signalled
--- new pidfile daemon=2182774                    ← a second daemon now owns the name
--- after killing s2, is the old daemon STILL alive? alive
```

### F3 — state files are removed by *name*, not by owner

`main.rs:133-134` (accept-loop cleanup), `main.rs:561-562` (`cmd_kill`) and `lib.rs:128-129`
(`recover_session`) all `remove_file(socket_path(name))` / `pid_path(name)` unconditionally — any of them
deletes whichever daemon currently owns that name, not the one that wrote the files.

```
=== attach exited rc=1
=== new exited rc=0            ← the pane re-created the session (F4)
=== state dir: s7.log          ← the dying old daemon unlinked its successor's sock+pid
--- orphan daemon 2185083: alive
--- ghostd kill s7 → ghostd: no session 's7'   exit=1
--- orphan daemon after ghostd kill: alive     ← unreachable and unkillable by the CLI
```

### F4 — the pane's script cannot tell "no session" from "session ended"

`remote.rs:191-194` builds `"$BIN" attach {s} 2>/dev/null || { "$BIN" new {s} >/dev/null 2>&1 && exec
"$BIN" attach {s}; }`, and `cmd_attach` exits **1** for every failure (`main.rs:375`, `:426`, `:433`,
`:481`). So a session that just *ended* — killed, or its shell exited — is re-created by the pane itself.
Measured twice: on `ghostd kill` (above) and by typing `exit`:

```
=== F4b.1 daemon=2187979; log before: 186 bytes   (typed: echo MARKER-BEFORE-EXIT, then exit)
=== F4b.2 s8 is BACK: old daemon 2187979 (dead), new daemon 2188104 (alive)
          log now: 633 bytes, MARKER-BEFORE-EXIT x2
```

A durable remote shell cannot be exited, and a killed session is immediately re-created.

### F5 — a liveness probe counts as an attach (found, not a symptom of this card)

`main.rs:77-84` sets `ever_attached = true` on `accept()`, before the SIZE handshake, so any probe
defeats the first-attach safeguard: `new s5 --first-attach-secs 3`, one probe, `sleep 5` →
`--- s5 daemon after the deadline (must be dead): alive`. Fixing it needs a protocol verb; see §9.

### F6 — a stale socket file makes the name permanently uncreatable, silently

`main.rs:358-364` calls `daemonize()` and then `UnixListener::bind(&sock)` — never
`ghostd::bind_listener` (`lib.rs:416-432`), the function that exists to clear a socket which refuses
connections. `bind_listener` is called from nowhere (`grep -rn bind_listener` → its definition only). And
because the parent has already exited 0, the child's bind failure is printed to `/dev/null` and lost.

```
=== SIGKILL the daemon; state left: s10.log s10.pid s10.sock
--- new s10 exit=0  (no output)
--- daemons named s10: 0 process(es)      ← new said "ok", no daemon exists
--- a client attaching: exit=1  ghostd: no session 's10'
```

### F7 — `kill` reports success it never verified

`main.rs:545-563` SIGKILLs and removes the state files without re-checking that the daemon died; the
shell side (`remote.rs:236-241`) ends with `|| true; true`, so the ssh child exits 0 even when nothing
was killed; `spawn_kill` (`remote.rs:418-437`) discards the outcome and the toast is unconditional
(`app.rs:1220`, `:1236`). The mechanism is fixed below (F2/F3 make a surviving daemon detectable); the
truthful toast is §9.

## 1. One root cause

A durable session's identity is a **name**, and its state (`<name>.sock`, `<name>.pid`, `<name>.log`) is
treated as belonging to the name rather than to the daemon instance that owns it. Nothing records "which
daemon wrote this", so (a) a new daemon inherits a dead one's log, (b) a wedged daemon cannot be killed
because the function that should kill it cannot parse the file that names it, (c) any daemon can delete
any other's state files, (d) a stale socket blocks the name forever, and (e) the pane cannot distinguish
"create the session" from "the session ended". Fix them as one idea — *state belongs to the daemon that
wrote it, and the daemon says why a connection failed* — not as seven patches.

## 2. The change set (stage 3 executes these; order = commit order)

### C1 — the log belongs to its daemon (`ghostd`, F1)
* `cmd_new`: **after** the probe (so a healthy session is never touched) and **before** binding, truncate
  the log: `let _ = fs::write(log_path(name), b"");` — with one `eprintln!` line when it discarded a
  non-empty log. A `new` session starts with no history.
* `cmd_kill`: after the daemon is confirmed dead, `let _ = fs::remove_file(log_path(name));`.
* Doc on both: replay is only ever the history of the daemon that is attached to; a session no daemon
  holds has nothing to replay.

### C2 — `recover_session` parses, kills and verifies (`ghostd`, F2)
Replace `lib.rs:110-130` with a `-> bool` ("the old daemon is gone"):
* read the pidfile through the new `read_pidfile` (below); no pidfile → `false` (nothing that names a
  victim — the caller reports it, it does not guess);
* `SIGTERM`, 1 s poll, `SIGKILL`, then `!pid_alive(daemon)` as the verdict;
* kill the recorded child's process group (`kill_child_group`, 2 s) — a killed daemon must not orphan the
  session's program (this is what `cmd_kill` already does at `main.rs:558-560`);
* remove the state only if the daemon is confirmed dead **and** the pidfile still names it (C3).
* `cmd_new` calls it and **dies** when it returns `false`:
  `die("'{name}' is held by a wedged daemon that will not die — kill it by hand")`. Never bind a second
  daemon over a live one.

### C3 — state files are removed only by their owner (`ghostd`, F3)
New in `lib.rs`, all `pub` and doc'd:
* `read_pidfile(name) -> Option<(Pid, Option<Pid>)>` — first line is the daemon, second (optional) the
  child; a file that does not parse is `None`.
* `pidfile_names(name, daemon) -> bool` — the ownership test.
* `remove_state_if_owned(name, daemon)` — removes `<name>.sock`/`<name>.pid` **only** when
  `pidfile_names` is true.
* `write_pidfile(name, daemon, child)` — write `<name>.pid.tmp`, then `rename` (a reader must never see a
  half file; today `main.rs:48-51` writes 15 bytes non-atomically).
* Call sites: accept-loop cleanup (`main.rs:133-134`) → `remove_state_if_owned(name, getpid())`;
  `cmd_kill` and `recover_session` → remove only after a confirmed death, only if the file still names
  that pid.
* `cmd_kill` with no pidfile must **not** delete a socket that still accepts a connection (that daemon is
  alive, it is just not named): try `UnixStream::connect`; connects → `eprintln!("'{name}' has no pidfile
  but its daemon answers — kill it by hand")` + exit 1; refused → the socket is stale, remove it.

### C4 — `new` binds before it daemonizes, through `bind_listener` (`ghostd`, F6)
`main.rs:358-364` becomes: `let listener = bind_listener(name).unwrap_or_else(|e| die(&format!(...)));`
**before** `daemonize()`, and the daemon then runs `accept_loop(listener, ...)` on the inherited fd
(`daemonize` only forks — no exec — so the fd survives; the socket is `CLOEXEC` but nothing execs).
Effects: a stale socket is cleared (that is what `bind_listener` is for), a live one is refused, and a
bind failure reaches the caller with a real exit status instead of vanishing into `/dev/null`.

### C5 — the daemon says *why* a connection failed (`ghostd`, F4)
New constants in `lib.rs`, doc'd as the contract with `src/remote.rs`:

| code | meaning | who returns it |
|---|---|---|
| `1` | there is no session (unchanged meaning — old app keeps working), **and, unchanged, a live session that is attached elsewhere (busy)** | `cmd_attach`: connect failed; busy: the refusal text as the handshake's first chunk (`main.rs:81`, `:441`) with **no `OK\n` seen**, so the relay's `Ok(0)` (`main.rs:481`) exits 1 |
| `3` | the daemon holds the session but never answered (wedged) | `cmd_attach`, the 8 s handshake deadline (`main.rs:423`, exit at `:426-427`) |
| `4` | the session exists but ended | `cmd_attach`: `Ok(0)` at the handshake (`main.rs:432-434`, the daemon accepted and closed before acking), and `Ok(0)` in the relay (`main.rs:481`) **after `OK\n` was seen** |

**Busy stays on exit 1 — decided here, not later** (stage-2 D1; the codes are asset-locked, §6).
Measured on this tree: the accept loop writes `busy: another client is attached\n` and returns, dropping
the stream (`main.rs:78-82`, the write at `:81`); `cmd_attach`'s handshake takes that text as its first
chunk and puts it in `out_buf` (`main.rs:438-441`, no `OK\n`), so the relay's first read is `Ok(0)` →
`exit(1)` (`main.rs:481`) before any flush — the text reaches nobody. The pane's script then falls into its
`|| { new … }`, which probes: `Probe::Healthy` → `die("…already exists")` (`main.rs:351`) → script exits 1 →
`remote.rs:601` (`auto_reconnect && code != 0`) retries, bounded by `reconnect_attempts = 5`
(`config.rs:84`, reset on success at `app.rs:1189`). So a transient overlap — a second app instance, or a
manual `ghostd attach` held during a reconnect — self-heals, and busy **cannot** create a session or
truncate the log, because C1's truncation sits *after* the probe. Mapping busy to 4 instead would hand a
*live* session the fate of a dead one (clean exit → `Manual`, zero retries); 4 must mean "it ended, stop
reconnecting" and nothing else. No new `eprintln!` for busy either: the pane discards the attach's stderr
(`2>/dev/null`), and detecting the string in Rust would couple the client to the daemon's English text
(§7.2).

The discriminator for 4 is the handshake, and busy cannot fake it: on success the daemon sends `OK\n`
(`main.rs:201`); on busy it *writes the refusal before dropping the stream* (`main.rs:81-82`), so a busy
client's first handshake read yields that text, never `Ok(0)`. Implementation: record whether `OK\n` was
seen. The relay's `Ok(0)` (`main.rs:481`) exits **4** only when it was (the session ended while we were
attached — the reported symptom) and **1** when it was not (busy: retry, never create). The handshake's own
`Ok(0)` (`main.rs:432-434`) stays **4** as planned: there the daemon accepted and closed before any ack, so
the connection carried a session that is over; it is not the busy shape, which always delivers bytes first.

### C6 — the pane creates only when the session is *missing* (`app`, F4)
`remote.rs:188-198` becomes (same `sq`/`dquoted` shape, same version check first):

```
"$BIN" attach {session} 2>/dev/null; rc=$?; \
if [ $rc -eq 1 ] || [ $rc -eq 3 ]; then \
  if "$BIN" new {session} >/dev/null 2>&1; then "$BIN" attach {session} 2>/dev/null; rc=$?; fi; fi; \
if [ $rc -eq 4 ]; then exit 0; fi; \
exit $rc
```

* `exec` is dropped so the *second* attach gets the same translation — otherwise a session that ends
  right after being created still reports 4 and the pane loops.
* **4 → exit 0 is the app-side signal**: `remote.rs:601` reads `auto = auto_reconnect && code != 0`, so a
  pane whose session ended is left disconnected (`Enter` reconnects). That is the whole reason a killed
  session stays killed — no `app.rs` change is needed, and none is made.
* **busy never enters this script as 4** (C5): it arrives as 1, the create branch runs, `new`'s probe
  answers `Healthy` and `new` dies without binding or truncating, so the script's status stays 1 and the
  pane's existing auto-reconnect retries it — the one path this card deliberately leaves as it is.
* A dropped ssh still exits 255 → auto-reconnect is untouched.

Doc comment on `attach_cmd` must state the contract: 1/3 create, 4 is an ending, nothing else creates,
busy is 1 on purpose (probe-guarded, retried), and no `exec`.

### C7 — journal
`PROGRESS.md`: one entry, the seven findings in two or three sentences, the measured numbers, and the
explicit "the daemon fixes reach no host until the assets are rebuilt" from §6.

## 3. Wire compatibility (what this does to the owner's hosts *today*)

| app | daemon | result |
|---|---|---|
| old (0.1.3) | old (0.1.3) | today's behaviour — the baseline the other rows are compared to |
| new | new (0.1.4, §6) | the fix |
| new | old (0.1.3, cached on every host) | **exactly today's behaviour** (no session → 1 → create; ended → 1 → create) |
| old | new | the old app **downgrades the asset back to 0.1.3** before its first pane, so it lands in the "new app / old daemon" row above — today's behaviour |

**How the `old | new` cell actually executes** (stage-2 D3). Not "the old script treats 4 as failure and
creates" — that path cannot run. The old app's `ensure_remote_binary` (`app.rs:1453-1470`, skipped once
`binary_ready` holds the host, `:1458`) calls `upload_ghostd` (`remote.rs:478-521`), whose `version_check`
(`remote.rs:450-455`) fails against a 0.1.4 binary, so it re-uploads its **own embedded 0.1.3 asset** over
it — a downgrade — and verifies it. From then on the old tab is talking to a 0.1.3 daemon and behaves like
`old | old`. Where that upload does not happen (it failed, or the host was already marked ready), the old
script's first act is still the version gate (`remote.rs:191-194`): `$($BIN --version)` ≠ the *app's*
version → prints "ghostd missing/stale —
reconnect to upload", **exit 1 before any attach** — which the pane reads as a failure and retries, never
as a 4. So the cell's outcome ("today's behaviour") stands; the mechanism is the asset version, not the
exit code. Two app versions alternating on one host therefore flip the cached binary's version back and
forth under the running panes — pre-existing (both sides key on the same string, by design), not
introduced here, and the reason the version must move in lockstep with the assets (§6).

The version check at the top of `attach_cmd` (`$BIN --version` vs `ghostd {CARGO_PKG_VERSION}`) is what
keeps the two sides in lockstep, and it is why **the version must not be bumped before the assets are
rebuilt** — a bump with a stale asset would make every durable pane refuse to attach
("ghostd missing/stale — reconnect to upload" forever).

## 4. Tests

`ghostd/tests/lifecycle.rs` (new file, 5 tests). Each test takes its own `GHOSTWAVE_STATE_DIR` under
`std::env::temp_dir()` and holds a **process-wide `Mutex`** while it does — the env var is global and
`set_var` is `unsafe` in edition 2024, so the safety comment goes on the lock. A `Drop` guard SIGKILLs
the pids it recorded and removes its directory. Binaries come from `env!("CARGO_BIN_EXE_ghostd")`.

| test | RED on `a91a1a8` | GREEN after |
|---|---|---|
| `a_new_session_does_not_replay_a_killed_ones_output` — new, attach-driver writes `echo MARKER`, kill, new, read `<state>/s.log` and one attaching client | log keeps the marker, client sees it (F1 measurement above) | log empty, client output has no marker |
| `new_recovers_a_wedged_daemon_and_leaves_one_daemon` — new, `SIGSTOP` the daemon, `new` again | old pid alive, second daemon binding | `new` exit 0, old pid dead, pidfile line 1 is the new pid |
| `a_session_that_ended_is_not_reported_as_missing` — attach a client, `ghostd kill` while attached | attach exits 1 | attach exits 4; `attach <no such name>` still exits 1 |
| `a_kill_leaves_no_state_behind` — new, kill | `<name>.log` survives | no `.sock`/`.pid`/`.log`; a later `new` works with an empty log |
| `a_busy_attach_is_not_reported_as_ended` — new; client A is a child `attach` whose stdin stays open (that is what makes the `client_tx` test at `main.rs:78` true), client B attaches | passes today (B exits 1) — a **pin, not a RED**: it exists so the exit-code refactor cannot quietly move busy onto 4 | B exits **1**, never 4; `<name>.pid` and `<name>.log` are byte-identical before and after, and A is still relaying (write a line to A's stdin, read it from its stdout) |

`ghostd/src/lib.rs` unit tests (2): `pidfile_names_reads_the_two_line_file` (two lines, one line,
garbage) and `state_files_are_removed_only_by_their_owner` (pidfile names 100; removal as 200 leaves both
files, as 100 removes both).

`src/remote.rs`: `StubRemote` (`remote.rs:1069-1105`) gains `attach_rc` and `new_rc` (its `attach`/`new`
arms currently `exit 0` unconditionally), and one new test
`a_pane_creates_the_session_only_when_attach_says_it_is_missing` asserts, for rc ∈ {1, 3} with `new`
succeeding: `record.new` exists and the final status is the second attach's; for rc ∈ {0, 4}: no create,
and the script's own status is 0 for 4. Its **busy-shape case** is the pin D1 asks for: `attach_rc = 1`
with `new_rc = 1` (exactly what `new` does when its probe answers `Healthy` — `main.rs:351`) must leave the
script's status **1**, so `remote.rs:601` auto-reconnects, and must not produce a status of 0. Two existing
textual assertions change: `remote.rs:880-883` (`exec` is gone; the create is now conditional on rc) and the
hostile-name test at `:1107-1126` (the stub's attach must exit 0 there, unchanged, and `injected` must
still not exist).

## 5. Verification (stage 3 runs these and pastes the real output)

```bash
export CARGO_TARGET_DIR=/var/build/ghostwave-t_6abf6154      # per card, never shared
# 1. baseline, measured on a91a1a8 BEFORE the diff, and pasted with the after-run:
grep -rh '^\s*#\[test\]' src/ | wc -l        # 266
grep -rh '^\s*#\[test\]' tests/ | wc -l      # 7   → 273 test functions
grep -rh '^\s*#\[test\]' ghostd/ | wc -l     # 0
# 2. app suite — expect base + 1 passed / 0 failed / 0 ignored (273 + the one new test);
#    the base number itself is taken from the run on a91a1a8 and pasted beside it
bash ~/.hermes/scripts/build_gate.sh nix-shell --run 'cargo test 2>&1 | tail -20'
# 3. daemon suite — expect 7 passed / 0 failed (5 lifecycle + 2 lib), 0 before
bash ~/.hermes/scripts/build_gate.sh nix-shell --run 'cd ghostd && cargo test 2>&1 | tail -20'
# 4. no new warnings
bash ~/.hermes/scripts/build_gate.sh nix-shell --run 'cargo clippy --all-targets 2>&1 | tail -20'
#    and, for the daemon:  cd ghostd && cargo clippy --all-targets
```

End-to-end, against a **native** build (`cd ghostd && cargo build` → `ghostd/target/debug/ghostd`), with
`GHOSTWAVE_STATE_DIR` in the scratch dir. Every assertion below is one of the measured blocks in §0,
re-run against the fixed binary; paste the raw output of each:

1. kill → `new` → an attaching client sees **nothing** of the dead session (F1).
2. wedged (`SIGSTOP`) → `new` → old daemon dead, exactly one daemon, a client can attach (F2/F3).
3. attached → `ghostd kill` → the attach process exits **4**, and the pane script (driven with a stub
   HOME holding the built binary) exits **0** without creating a session (F4/F7).
4. `exit` typed in the durable shell → no new daemon appears (F4b).
5. `kill -9` the daemon → `new` binds a fresh daemon and a client attaches (F6).
6. after 3–5: `ls $GHOSTWAVE_STATE_DIR` — no `.log`, no `.pid`, no `.sock` left behind.
7. busy: `new`, an attach held open, a second `attach` → the second exits **1** (not 4), the first still
   relays, and `<name>.pid`/`<name>.log` are unchanged; the C6 script run against that state exits **1**
   (the `new` it falls into dies with "already exists") so the pane retries, and **no** daemon or session was
   created.

**Never run any of this without `GHOSTWAVE_STATE_DIR` set.** Two of the owner's live sessions are running
on this host (`ghostwave-orac`, `ghostwave-death916orac` — visible in `ps`); a bare `ghostd kill-all`
would destroy them.

## 6. Delivery — what stage 3 does NOT deliver (and why that is a separate card)

The daemon that runs on a host is the **embedded static asset** (`include_bytes!("../assets/ghostd-…")`,
`remote.rs:46-49`), and the app only re-uploads when `$BIN --version` differs from its own
(`remote.rs:191-194`, `:450-455`). So every fix in `ghostd/src` is **inert on every host** until:

* both manifests go to `0.1.4` (`Cargo.toml:3`, `ghostd/Cargo.toml:3`), **in the same commit** as
* `assets/ghostd-x86_64` and `assets/ghostd-aarch64` rebuilt by `scripts/build-ghostd.sh both`
  (`scripts/build-ghostd.sh:1-30`), and the app rebuilt to embed them.

That cannot be done on orac, and the closest-looking workaround is a trap worth naming (stage-2 D4). The
script's NixOS path is a **hosted x86_64 1.86.0 toolchain**: `RUST_VERSION="1.86.0"`
(`scripts/build-ghostd.sh:21`), `nixos_setup` fetches and patchelf's `rustc`/`cargo` for
`x86_64-unknown-linux-gnu` (`:71-77`), and `nixos_build` copies `rust-std-1.86.0-<target>` into **that
toolchain's own sysroot** — `SYSROOT="$tc/rustc-1.86.0-x86_64-unknown-linux-gnu/rustc"` (`:61`), the copy at
`:98-101` — then links with `$SYSROOT/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld` (`:110`). None of
it can execute on `uname -m` = `aarch64`. The nix `rustc` here is **1.95.0**, measured in this repo:
`nix-shell --run 'rustc --version'` → `rustc 1.95.0 (59807616e 2026-04-14)`, sysroot
`/nix/store/8865lxjk610n0n0sjsgf11599gq37i3g-rustc-1.95.0`, whose `lib/rustlib` holds only
`aarch64-unknown-linux-gnu` and the wasm targets — **no** musl std. A `rust-std` is loadable only by the
`rustc` of the *same version*, so 1.86.0's tarballs are valid only inside the 1.86.0 toolchain the script
fetches itself; dropping them into the 1.95.0 nix sysroot cannot link. So stage 3 does **not** bump any
version and does **not** touch `assets/`: a bump with a stale asset bricks every durable pane (§3).

Stage 3 creates the follow-up card, `NEEDS-VULCAN:` rebuild the assets at 0.1.4 — one command on any
x86_64 host with rustup. It also records two **unverified** alternatives (do not trust them until measured;
each pairs a std with the rustc that loads it): for the **aarch64** asset on orac,
`nix-shell -p pkgsMusl.rustc pkgsMusl.cargo --run 'cd ghostd && cargo build --release'`; for the **x86_64**
asset, a `rust-std` matching the rustc it will load (as measured here that means
`rust-std-1.95.0-x86_64-unknown-linux-musl` with a 1.95.0 x86_64 rustc — never the script's 1.86.0 std into a
1.95.0 sysroot) plus `pkgsCross.musl64.stdenv.cc` as the linker.

**Stage 3's summary must say this plainly**: source fixed and verified; no host behaves differently until
that card lands.

## 7. Risks / traps

1. **Destroying the owner's live sessions.** `GHOSTWAVE_STATE_DIR` or nothing; `kill-all` only ever
   inside a scratch state dir. (§5.)
2. **Old daemon, new app** = today's behaviour, on purpose (§3). Do not "fix" that by message-grepping
   `attach`'s stderr: it couples the script to an English string.
3. **`bind` before `daemonize`** moves where the socket is created. It works because `daemonize` forks
   only (no exec, so `CLOEXEC` never fires) — but the first lifecycle test must prove a client can attach
   through it, not assume it.
4. **Truncating the log on `new`** is a real behaviour change: history is now only replayable while its
   daemon lives. That is the point of the card, but say it in PROGRESS.md — an `attach` to a *live*
   session still replays everything.
5. **The ssh hop is not verifiable here — for want of a key, not of a daemon** (stage-2 D2, re-measured by
   this seat). The assumption: `ssh -t … sh -c` returns the remote command's status as the pane's exit code,
   and `last_exit_code != 0` drives the reconnect pass (`remote.rs:600-601`). Measured on orac: sshd **does**
   listen — `ss -tln` → `LISTEN 0.0.0.0:22` and `[::]:22`; `ps` → `sshd: …openssh-10.5p1… [listener]`
   (root); `journalctl --follow -n 0 _SYSTEMD_UNIT=sshd.service` running. The blocker is authentication:
   `ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new localhost true` →
   `death916@localhost: Permission denied (publickey).`, rc 255 — publickey only — and
   `~/.ssh/authorized_keys` does not exist (`~/.ssh` holds only `hermes_ed25519`, `hermes_ed25519.pub`,
   `known_hosts`, `agent/`). **Stage 3 runs that probe first.** If it still fails, stage 3 states the
   assumption and marks the end-to-end claim `NEEDS-OWNER-VERIFY` with exactly this reason ("no authorized
   key for localhost; sshd listens on :22, publickey-only"), plus the one-line owner action that would make
   it verifiable here: append `~/.ssh/hermes_ed25519.pub` to `~/.ssh/authorized_keys`. The sentence "no sshd
   listens on orac" is false and must not reach C7's journal or `PROGRESS.md`.
6. **Parallel tests and the env var**: one `GHOSTWAVE_STATE_DIR` per test, a process-wide mutex around
   it, `unsafe { env::set_var }` with a safety comment (edition 2024).
7. **Two-line pidfile readers**: `read_pidfile` is the only reader; do not leave `cmd_kill`'s inline parse
   (`main.rs:535-540`) duplicated after the refactor.
8. **`busy` is exit 1 on purpose** (D1; the reasoning is in C5). Giving it 4 would hand a *live* session the
   fate of an ended one — script exit 0 → `Manual`, zero retries — where today the pane probe-guards its way
   into the existing retry loop (bounded by `reconnect_attempts` = 5). If "attached elsewhere" should ever be
   visible, it needs a distinct code **and** a script branch that retries without creating: its own card, not
   a quiet change here. Wire codes are asset-locked (§6), so this decision cannot be revisited cheaply after
   the 0.1.4 rebuild. Held by the `a_busy_attach_is_not_reported_as_ended` lifecycle test and the busy-shape
   case of the app test (§4).

## 8. NOT doing (explicit)

* **No version bump, no asset rebuild** on this card (§6) — the follow-up card owns them.
* **F5** (a probe counts as an attach): needs a new protocol verb plus a version handshake to be
  version-safe; a safeguard erosion, not one of the reported symptoms. Own card.
* **The kill toast / exit status**: making "Killed remote session" truthful needs the ssh child's outcome
  carried into the tick loop (`spawn_kill` is fire-and-forget, `kill_cmd` ends with `true`,
  `remote.rs:236-241`). Own card.
* **Naming sessions** (so two durable sessions can exist on one host). The name is derived from the host
  on purpose (`term::remote_session`); this card makes *the* session on a host recoverable, it does not
  add a second one.
* **`pkill` sweeping a daemon that has no pidfile at all** from the CLI. After C3 a live daemon always
  owns its pidfile, so this is only reachable for daemons started by the *old* binary; the app's kill path
  already sweeps them (`kill_pattern`).
* **A distinct "attached elsewhere" exit code for `busy`** (§2 C5, §7.8): busy keeps exit 1 and the pane's
  existing retry, and no message is added for it. Making the overlap visible, or retrying explicitly on a
  distinct code, is its own card.
* No touch to the pty/term/search subsystems, no keymap or palette changes, no UI.

## 9. Ordered work items for stage 3

1. `ghostd/src/lib.rs`: `read_pidfile` / `pidfile_names` / `remove_state_if_owned` / `write_pidfile`, the
   three exit-code constants, and the rewritten `recover_session` (C2, C3, C5) + the two unit tests.
2. `ghostd/src/main.rs`: `cmd_new` (probe → recover-or-die → truncate log → `bind_listener` → daemonize),
   accept-loop cleanup and pidfile write through the new helpers, `cmd_attach`'s exit codes (1/3/4 — the
   relay distinguishes them by whether `OK\n` was seen, and busy stays **1** with no new message), `cmd_kill`
   (verify death, owned removal, log removal, no-pidfile rule) (C1–C5).
3. `ghostd/tests/lifecycle.rs` — the five tests (C2–C5 plus the busy pin); run `cd ghostd && cargo test`
   through the gate.
4. `src/remote.rs`: the new `attach_cmd` script + doc comment, `StubRemote`'s `attach_rc`/`new_rc`, the new
   test (including its busy-shape case), and the two existing assertions (C6).
5. `cargo test` + `cargo clippy --all-targets` for both crates through the build gate; paste the counts.
6. The seven end-to-end checks of §5 against `ghostd/target/debug/ghostd`; paste the raw output.
7. `PROGRESS.md` entry (C7), then commit, push the branch with `fleet_push.sh`, open the PR, create the
   `NEEDS-VULCAN:` asset follow-up card, and hand stage 4 to `review`.

## 10. What stage 2 has to judge

1. **Is the root cause right?** F1/F2/F3/F4/F6 are reproduced in §0 with the code anchor beside each one.
   The load-bearing claim is F3: *any* daemon deletes *any* daemon's state files, which is what leaves a
   live daemon with no `.pid`/`.sock` (measured). Challenge it if C3 does not actually close it.
2. **Is C4 safe?** binding before `daemonize` is the one structural change; the plan gives its reason
   (F6's silent bind failure) and its only evidence requirement (a lifecycle test that attaches).
3. **Is C5/C6's contract right** — 1 keeps its old meaning so an old daemon is not broken *and* stays the
   code for `busy`, 4 maps to exit 0 in the script because `remote.rs:601` treats a clean exit as "do not
   reconnect", and no `app.rs` change is needed? Is any path left where a killed session still gets
   re-created, and is any path left where a *live* session stops retrying?
4. **Is the scope split honest** — source now, assets by a `NEEDS-VULCAN:` card, and a summary that says
   no host changes behaviour until then?
5. **Are the numbers right** — 273 → 274 in the app, 0 → 7 in `ghostd` (5 lifecycle + 2 lib), and are the
   RED expectations for all seven new tests actually red on `a91a1a8` (the busy pin is the one exception: it
   guards a status that must *stay* 1, so it passes on `a91a1a8` too and is labelled as a pin)?
