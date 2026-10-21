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
