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

# PLAN — a pane's canvas must not paint over its frame stroke

relay: job=t_b6e8cb0c · stage 1 (PLAN), **revision 2** (this card: `t_90db7f5b`) · stage-2 verdict
that produced it: changes-required on `t_90130166` · branch `wt/t_b6e8cb0c` (based on `master` @
`a9c2f97`) · owner report: "pane still overlaps border. the canvas seems to go over the border line
but you can see a small bit of it on the left" · screenshot `https://ibb.co/VWvPmKyj`

This card arrived from the dashboard with **no body and no plan stage bought**; the previous card in
this series (`t_2a5f5dab`, PR #16, merged as `46c1ce1`) fixed the *missing* frame line where two
panes touch. This one is the residual report: the frame line is there, but a pane's **canvas paints
over it**.

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
   phase) / `≥ 18` (any phase) at the default `padding = 6.0` — and at any font at all if the owner
   runs a small `padding`.
3. **Withdrawn: the whole-paint `with_clip`.** Stage 2 called it a crop of the marker; measured, it
   is worse — at the defaults the marker's ink lies **entirely outside** the canvas
   (canvas-local `−1.2 … −0.3`), visible only because iced gives the canvas no clip, so
   `with_clip(ORIGIN .. bounds.size())` around `paint_pane` would **delete the search marker**. The
   clip is replaced by a clamp on the one primitive that leaves (§3).
4. **§9 is asked with this card**, not after stage 3: at the defaults nothing in `paint_pane` touches
   a stroke, so the screenshot's element is still unidentified and only the owner can identify it.

§5 is still the acceptance; §10 lists what stage 2 has to judge this revision on.

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
| cursor block / bar / underline | `src/pane_canvas.rs:296-320` | `ox + vp.x·cw`, `vp.x < cols` for the snapshot it painted | **no** — inside the text area |
| **search-row left marker** | `src/pane_canvas.rs:285-295` | `x = ox − cw = rect.x + p − cw`, ink `x … x + cw/8` | **yes**, when `cw > p + inset + rect.x` |

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
| 12 (default) | 7.2 | **+2.8 px** | 1.2 px left of the canvas edge, inside the gutter — invisible, no stroke contact |
| 15 | 9.0 | +1.0 px | gutter, 1 px short of the stroke |
| 16 | 9.6 | +0.4 px (worst phase `−0.1`) | touches the stroke only at `rect.x ≈ −0.5` |
| 17 | 10.2 | **−0.2 px** | 0.2 px of `search_match` orange on the 1 px stroke |
| 18 | 10.8 | −0.8 px | covers the unfocused stroke, reaches the cell edge |
| 20 | 12.0 | −2.0 px | past the 1 px stroke and past the cell edge, into the neighbour's gutter |
| 24 | 14.4 | −4.4 px | 3.4 px into the neighbouring cell |

The threshold moves with the configured padding: `cw > padding + 4.0 + rect.x` — `padding = 4` opens it
at `font_size ≥ 13` (best phase), `padding = 2` at `font_size ≥ 10`, `padding = 0` at any font. A
configured font family (`ui_fonts::init`, `src/ui_fonts.rs:19-45`) changes the ratio and moves the
threshold with it; the fix in §3 does not depend on the ratio.

The marker is **one row tall and `cw/8` wide** (0.9 px wide, one cell row tall, at the defaults). So: a real, visible
overhang defect — but a per-row sliver, not the full-height line at `x≈1424–1427` in the screenshot.

## 2. Candidates, ranked, with the evidence that decides them

| # | Cause | Evidence | Verdict |
|---|---|---|---|
| C1 | background fill overhang from `origin_offset` (`§1.1`) | ≤ 0.5 px into a 4.0 px `theme.bg` gap, same colour | **cannot produce any visible pixel** — hygiene only |
| C2 | the terminal keeps one more row/column than fits | `cols = ⌊(w − 2p)/cw⌋` → the last cell ends ≥ 5.5 px inside | **ruled out** by arithmetic (`bounds_to_cols_rows_calculations` already covers the fit) |
| C3 | the search rows' gutter marker (`§1.2`) | `x = ox − cw`; crosses the inset at `cw > p + inset + rect.x`, measured `font_size ≥ 16…18` at defaults, `≥ 10` at `padding = 2` | **the only primitive in `paint_pane` that reaches a stroke** — real, configuration-dependent, not the screenshot |
| C4 | the reported line is not a pane frame at all but chrome/overlay (tab bar, status bar, search overlay, sessions modal), or something outside this tree | nothing in `master` places a full-height line at the image's right edge (`§0`) | **only the owner can say** — §9, asked with this card |

## 3. Decision — design for stage 3

Two changes; the first is the defect fix, the second is hygiene that makes "a pane paints only inside
its canvas" true by construction.

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
* Everything after PR #16 must stay green: `framed_panes_carry_their_frame_on_shared_edges`,
  `frameless_panes_touch_with_no_gutter`, `click_to_pane_index_mapping`,
  `stack_label_string_formatting`, `bounds_to_cols_rows_calculations`.

## 5. Verification (stage 3 runs these and pastes the real output)

Through the build gate, per-card target dir, never a bare `cargo`:

```sh
CARGO_TARGET_DIR=/var/build/ghostwave/t_b6e8cb0c \
  bash ~/.hermes/scripts/build_gate.sh \
  nix-shell --run 'cargo test --locked panes_grid::'
# expect: 6 passed / 0 failed  (5 existing + paint_rect_stays_inside_the_canvas)

CARGO_TARGET_DIR=/var/build/ghostwave/t_b6e8cb0c \
  bash ~/.hermes/scripts/build_gate.sh \
  nix-shell --run 'cargo test --locked pane_canvas::'
# expect: 1 passed / 0 failed  (the new marker test; pane_canvas has no other unit tests)

CARGO_TARGET_DIR=/var/build/ghostwave/t_b6e8cb0c \
  bash ~/.hermes/scripts/build_gate.sh nix-shell --run 'cargo test --locked'
# expect: 220 passed / 0 failed / 0 ignored (218 today — PROGRESS.md:1377 — + the two new tests)

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
search marker moving 1.2 px right (cell-local 3.8 → 5.0); nothing else should look different. At
font_size >= 17 a search row's orange gutter marker used to sit on the pane's frame stroke and must
now stop at the pane's edge. The owner's reported overlap is NOT addressed by this diff (PLAN.md §9)
and needs an unscaled capture to identify.
```

## 6. Risks / traps

* **The owner's symptom may not be in this diff at all.** At the defaults no `paint_pane` primitive
  touches a stroke (§1.1), so the screenshot's element is something else (§0, §2 C4). The answer to
  §9 decides whether it becomes its own card; this plan does not chase it.
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
* No merge, no push to `master`; the PR is stage 3's, and `death916/ghostwave` is public so the merge
  is the owner's.

## 8. Ordered work items for stage 3

1. In `/home/death916/code/ghostwave/.worktrees/t_b6e8cb0c` (branch `wt/t_b6e8cb0c`, already merged
   up to `master` @ `a9c2f97`), confirm `git status` is clean and `git log -1` is this plan commit.
2. Paste the RED evidence for both tests before adding either helper: the current inline rect math
   for `paint_rect_stays_inside_the_canvas`, and `padding − cell_w` for
   `search_marker_stays_inside_the_canvas` (§4). Run `panes_grid::` through the gate, record the
   failures, then delete the temporary copies.
3. Add `fn marker_x(padding: f32, cell_w: f32) -> f32` in `src/pane_canvas.rs` with the doc comment
   and use it at `:289`; add its test.
4. Add `fn paint_rect(bounds: Rectangle) -> Rectangle` in `src/panes_grid.rs` with the doc comment and
   use it in `draw()`; add its test.
5. Run §5's four commands; paste `N passed / M failed`, clippy rc, and `rustfmt --check` on the two
   touched files (this module has pre-existing import-order hunks — say so, do not "fix" them).
6. `PROGRESS.md`: one dated entry in the repo's running-log style, recording the revision-2
   arithmetic (4 px inset, 0 visible px for the fill, the marker threshold, the rejected clip).
7. One commit for the behaviour (`fix(panes): keep a pane's canvas inside its frame`), push the
   branch through `~/.hermes/scripts/fleet_push.sh dev "$PWD" origin wt/t_b6e8cb0c`, open the PR
   against `master` and read it back (state, head sha, base, mergeable).
8. Mark the card `NEEDS-OWNER-VERIFY` with §5's wording (headless box) and hand stage 4 to `review`.

## 9. OWNER QUESTION — asked with this card, not after stage 3

At the defaults nothing in a pane's `paint_pane` touches a frame stroke (§1.1), so the element in the
screenshot is still unidentified. One answer decides whether the remaining symptom is a pane frame,
the marker path above, or chrome/overlay:

> Which border in `https://ibb.co/VWvPmKyj` is the one being covered — a **pane frame** (the 1–2 px
> stroke inside the pane's own edge, i.e. between two panes or around a single pane), the **tab bar's
> or status bar's** border, or the **search / sessions overlay** edge — and is the window a **2-pane
> side-by-side split** at the time? A full-resolution (unscaled) screenshot of just the ghostwave
> window would settle it in one look; the supplied image is resampled to 1492x922 and carries no
> theme frame colour at all (§0), so it cannot be measured to the pixel.

Also needed with the answer, because they set the threshold in §1.2: the owner's `font_size`,
`padding` and `show_frames` from `~/.config/ghostwave/config.toml` (defaults are 12.0 / 6.0 / on —
`src/config.rs:157,161`).

What the answer buys: "a pane frame" → stage 3's marker clamp plus the snap is the whole fix and the
symptom should be re-checked at the owner's font size. "Chrome or an overlay" → the marker clamp
still lands (defence in depth) and the reported element becomes its own card with the capture as its
evidence. Either way, **stage 3 must not be sold as closing the owner's report** until the capture
comes back.

## 10. What stage 2 has to judge this revision on

1. Is §1.1's table complete and right — is the marker really the only primitive that can leave the
   canvas, and are the four "no" rows' bounds right?
2. Is §1.2's threshold arithmetically correct (`cw = 0.600 · font_size`, `cw > padding + inset +
   rect.x`), and is it honest to call this a real defect at `font_size ≥ 16…18` rather than a
   default-config one?
3. Is the clamp the right instrument, and is rejecting `with_clip` justified by the measurement
   (marker ink entirely outside the canvas at the defaults) rather than by preference?
4. Is the plan honest about the visible symptom — that it is not closed by this diff and needs the
   owner's capture (§9)?
