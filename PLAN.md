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
