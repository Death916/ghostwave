# ghostwave — build progress

waveterm-style terminal built on **libghostty-vt** (the Rust crate from
`Uzaaft/libghostty-rs`) + macroquad, with **zellij-style keybindings**.
Durable sessions (pin tabs, scrollback restore) implemented — see the
README.

Project root: repo checkout.
Binary name: `ghostwave`

---

## STATUS: main terminal features solid — render/mouse/keybinds fixed, parity work in progress

> **Where the front-end lives now (2026-09-17):** the iced build *is* the root crate — `src/main.rs`
> and the modules beside it — and `ghost-iced/` no longer exists. The journal below is ordered
> oldest-first, so entries refer to paths as they were when written. The retired macroquad front-end
> is archived on branch `macroquad-archive` / tag `macroquad-final`.

**iced front-end WIP (branch `feat/iced-frontend`)**: a second front-end in

`ghost-iced/` renders a full tab (panes, frames, tab bar, status bar, wave, toast) from the same
terminal core, drives every keymap mode, and — as of M5a (parts 1 & 2) — keeps its layout and view
in iced's own `pane_grid::PaneGrid`, with per-cell canvas, rounded-origin subpixel anti-blur,
stack-aware title bars, dynamic sizing from iced bounds, and warning-free compilation (suite green;
the count is recorded per commit in the sections below). Not merged yet.
**The macroquad front-end is being retired**: archived to a `macroquad-archive` branch plus a
`macroquad-final` tag and removed from the ongoing branches once the iced build covers its feature
set (`RETIREMENT-PLAN.md` §2). Its features are the gate until then; its looks are not the reference.
**Start with `ghost-iced/HANDOFF.md`** (brief for the next session), then
`ghost-iced/PORT-PLAN.md` §13 (status), §16 (direction change), §17 (Plan A + the M5a parity
table).

### iced front-end, M0 (branch `feat/iced-frontend`) — second front-end builds and runs, one pane
New crate `ghost-iced/` (iced 0.14 + libghostty-vt, path-dep on the app lib, `[workspace]`
marker so it never joins the app package; `ghost-iced/PORT-PLAN.md` is the port plan).
The macroquad front-end is untouched and still the reference; both run side by side.
- **Moves untouched**: pty, session, layout, config, theme, search, palette, clipboard are
  imported from the app crate, not copied. `TermPane` (pty + VT + scrollback + selection +
  key encoding + pane log) is the pane; the macroquad `render()` is simply never called.
- **The pane's render scratch (`RenderState`/`RowIterator`/`CellIterator`) is private**, so
  the iced painter owns a second set driven off the pub `term` field. Its pty receiver moves
  to a reader→iced bridge thread (`pty_bridge.rs`, 256 KiB coalescing), so pty output is an
  iced event instead of a poll; `pane.rs::ingest`/`exited` re-do what `poll_pty` does for the
  macroquad build (pane log, `vt_write`, EOF/reap).
- **Grid follows the window** via `iced::window::resize_events`, with the app's own formula
  `((extent - 2*padding) / cell_extent).max(1)`; verified on the live pty with a fixture that
  printed `stty size`: `30 100` at spawn → `68 140` after the compositor tiled the window.
- **Two text bugs found and fixed — both were wrong defaults, not port work**:
  (1) the canvas was drawing with `Font::DEFAULT`, i.e. iced's built-in proportional Fira Sans,
  while the cell grid is measured from JetBrains Mono at 12px — this is what made glyph
  spacing look uneven. The face must be registered (`iced::application(..).font(bytes)`) and
  named in the canvas `Text`. (2) `Shaping::Basic` is cosmic-text's *"shaping with no font
  fallback"* (`shape_fallback` is only reached on the advanced path), so no amount of
  registered fallback fonts helps a glyph the primary face lacks — starship's `❯` (U+276F),
  braille, nerd-font icons all stayed tofu until the canvas switched to `Shaping::Advanced`
  (what iced's own text pipeline uses everywhere). Fallback chain mirrors the macroquad
  stack via `fc-list` (Nerd Font Mono → DejaVu Sans Mono → Noto Sans Mono → …).
  Side effect to watch: Advanced shaping applies the font's default features, so JetBrains
  Mono's `->`/`=>` ligatures may appear (macroquad draws per glyph and has none).
- **Text runs**: consecutive same-colour cells are drawn as ONE canvas text item (mirroring
  `term.rs`'s `CellRun`/`flush_run`), so glyphs advance by the font's 7.2px advance rather
  than being pinned to the rounded-up 8px cell — one text item per cell leaves a visible gap
  after every character. Underline/strikethrough are drawn; per-cell bg rects are skipped
  when they equal the terminal default (~2000 fewer rects/frame).
- **Not implemented yet** (plan milestones M1–M6): multi-pane/tab chrome, keymap modes,
  mouse, scrollback wheel, selection/copy, search overlay, palette, preview/visualizer,
  kitty graphics. `font_size`/`theme`/`padding`/`row_gap`/`scrollback` come from
  `config.toml` and the theme's fg/bg are applied to the pane like `apply_terminal_colors`.
- Known shared gap: emoji stay tofu in BOTH front-ends (the only emoji font here is Noto
  Color Emoji, a CBDT colour-bitmap face neither renderer can rasterise; `fonts.rs` skips
  colour fonts deliberately). Cursor is composited behind its glyph under iced (text layer
  sits above shapes) — visible only when the cursor is on a glyph.

### iced front-end, M1 + M2 (branch `feat/iced-frontend`) — multi-pane, frames, tab bar, status bar
The iced front-end now renders a full tab: N panes from `layout::layout`, pane frames
and labels, the tab bar, the status bar (mode pill, hints, right-aligned theme name,
sync chip) and the toast box. The macroquad front-end is untouched and still the
reference; both were captured at the same window size and compared.
- **M1**: `ghost-iced/src/app.rs` holds tabs + the split tree + cached pane rects.
  Splits/closes/focus go through `layout::split_at` / `close_at` /
  `focus_in_direction` — no layout math duplicated. Verified live: 3 panes → 3 fish
  children of the iced process, each with its own pty bridge and pane log (a fixture
  piping `seq 1 30000` into two panes produced two ~196 KB pane logs). Each pane is
  painted inside `Frame::with_clip` — one canvas for the window, so rects stay in the
  app's coordinate scheme while clipping replaces the macroquad build's GL scissor.
  Pane ids are stable and closed panes leave a tombstone (compacting the vec, as
  `App::remove_dangling_panes` does, would renumber panes under the `(tab, pane)` tag
  their pty bridge already stamped on the stream).
- **M2**: chrome painted per `ghost-iced/CHROME-REFERENCE.md`, a line-cited spec
  extracted from `main.rs`'s render path. Chrome is painted into the same canvas as the
  panes in the reference's order (panes → frames → tab bar → status bar → toast), which
  keeps geometry and theme fields identical to the reference. Canvas text cannot be
  measured, so `measure.rs` measures it with fontdue from the same font file
  (macroquad's `measure_text` sums per-char advances) and converts the spec's baselines
  to iced's top-left origin; every box width is `text_width + pad`, as in the reference.
  Tab clicks hit-test at the renderer's 130px chip width (the reference's own hit test
  uses 120 — a real 10px mis-selection bug not copied).
- **Animation is a conditional tick**, not a frame loop: 16 ms while the wave is
  energized or a toast is up, 125 ms while the wave idles (matching the macroquad idle
  tier, at the cost of 8 presents/s rather than 0), and no tick at all when
  `visual_wave = false`. `iced::time::every` does not exist in 0.14, so `tick.rs` runs a
  small publisher thread, the same shape as the pty bridges; the reference's frame
  counts became real durations.
- **Verified by capture at the same window size** (1133x1382, both builds): tab chip
  with the pane's OSC title (`/tmp`) on `theme.tab_active` over a `theme.tab_bg` strip,
  status bar with the `NORMAL` pill, the exact hint string, the right-aligned theme
  name, and three framed panes with labels. **No wave in either build** — with the
  115-char NORMAL hints (~897px) plus the mode pill and margins, the reference's own
  guard (`wave_end_x > wave_start_x + 50`) fails at this width, so the wave only
  appears on wider (maximized) windows: parity, not a port gap, but the wave's drawing
  is still unexercised.
- iced renders every text item above every shape in one `Frame`, so the pane
  frame/label pass is clipped to the content rect — otherwise a top-row pane label
  floats over the tab bar instead of being covered by it (which chrome-last drawing
  achieves in the macroquad build).
- Shared-`XDG_DATA_HOME` hazard: `session::prune_logs()` (run when any instance deletes
  a session) removes pane logs no saved session references, so a macroquad instance can
  unlink the iced instance's LIVE logs (seen as `(deleted)` in `/proc/<pid>/fd`). Test
  the iced build with `XDG_DATA_HOME=/tmp/ghost-iced-data`; `GHOST_ICED_PANES=<n>` and
  `GHOST_ICED_FIXTURE=<cmd>` make layout/multi-pane/log behaviour testable without
  typing. (The M1-era Alt+Shift binds were deleted in M3 — see below.)

**M3 LANDED — the keymap modes are real (hotkeys work).** `Ctrl+p t n s h o g` enter modes,
the status pill and hint line change with the mode, every mode's binds work, `Esc` exits a
modal mode, `Ctrl+q` quits. The bind table is still the app crate's `keymap`; the new
`ghost-iced/src/input.rs` is only the mapping from a `Dispatch` onto this front-end's state
(`execute` mirrors `App::execute`). Three things decided on the way, all easy to trip over
later:

- **Panes now have an identity, not a position** (`pane::PaneId`, a process-wide counter,
  carried by each pane's pty bridge; output/exit messages route through
  `AppState::pane_by_id`). Index-based `(tab, pane)` tags broke the moment `close_tab`
  (shifts the tab vec down) and `break_pane` (moves a pane to a new tab) existed, because a
  bridge subscription cannot be re-keyed after it takes its receiver.
- **Split semantics**, since this got misremembered in conversation: `SplitDir::Horizontal`
  = side-by-side (new pane to the RIGHT), `Vertical` = one above the other (BELOW). Pane
  mode `d`/`n` = down, `r` = right, `s` = stacked; global `Ctrl+d` = right,
  `Ctrl+Shift+d` = down.
- **Repeats are dropped in modal modes.** The reference forwards a held key's repeats
  straight to the pane without consulting the keymap, which types into the shell while the
  status bar says PANE; here repeats only flow in Normal and Locked.
- Unfinished features now **toast** rather than silently doing nothing (floating panes,
  scratchpad, palette, sessions, search, preview/AI/visualizer, remote panes).

Verified by eight acceptance tests in `input.rs` that press the keys through the real
dispatch path with real spawned panes and assert the resulting state, plus three in
`fonts.rs` — 13/13 pass (`cargo test`, command in `ghost-iced/PORT-PLAN.md` §15). Not
covered by tests: that winit delivers the chord to the window, and anything visual.

**DIRECTION CHANGE (2026-09-16): iced-native, not macroquad parity.** The user's call — they prefer
iced's text rendering, know iced better, and want iced's built-ins used instead of hand-rolled
equivalents. So `pane_grid` replaces the hand-painted pane layer (divider styling, click focus,
drag-resize, one canvas per pane, and no more "who paints on top" bugs — the invisible-frame
regression fixed today was exactly that class), and the chrome/overlays move to real widgets,
which retires most of `measure.rs`. `ghostwave::layout` stays as the persisted session
representation and for stacks/floats, which pane_grid has no equivalent of. Plan reshuffle:
M4 tabs + sessions, then **M5 = pane_grid migration**, M6 extras, M7 demoted to perf numbers only
(visual parity is no longer a criterion). Detail: `ghost-iced/PORT-PLAN.md` §16.

**NEXT UP — M4: tabs and sessions.** Tab ops are done (new/close/next/prev/goto/last/rename/
move via `Ctrl+t` and the globals); what M4 adds is `session.rs`-backed save/restore, the
`Ctrl+o` session manager overlay (list, switch, new, rename, delete), the last-session
marker, and the save-on-quit path the quit action currently skips. Then M5 (leftover config
knobs + runtime theme switch), M6 (selection/copy, search, palette, preview/visualizer,
kitty graphics last), M7 (side-by-side parity + idle-perf numbers before any
default-binary flip). Rationale and the module map: `ghost-iced/PORT-PLAN.md` §13 and §15.

**Convention, applies to the whole port**: Rust sources carry only rustdoc (`//!`, `///`)
written for a caller; everything that explains a decision, cites another file, records
history or addresses a future maintainer goes in these markdown files instead.

The user's read on the two builds: iced's text looks sharper and is preferred, and the port
stays reversible — a wipe and continue with macroquad remains a live option until iced
reaches parity.

### iced front-end, M3 + M5a (branch `feat/iced-frontend`) — input parity, pane layer, and pane view in iced

**M3 (input parity)** — `input.rs` mirrors the reference's `keymap::handle`/`execute` as an iced
message handler: pane/tab/resize/move/scroll/lock modes, the globals, `Ctrl+q`, and a toast for
everything not ported yet (palette, search, sessions). Repeats are platform events now (no
synthetic repeat timer) and a release is forwarded only when the press was not consumed. 13
acceptance tests drive real panes through `input::key_press`; there is no key-injection tool on
Hyprland 0.5x, so those tests are the verification.

**M5a part 1 (the pane layer)** — a tab now owns `pane_grid::State<GridPane>` (payload
`{ panes: Vec<PaneId>, active }`, so a zellij stack is one cell holding several ids) and every pane
operation routes through iced: `split` / `close` / `adjacent` / `resize` / `maximize` / `restore` /
`swap` / `drop`. `AppState`'s rect cache, `tree_revision`, `sync_grid` and the derived `GridCache`
layer are deleted; `ghostwave::layout::Node` survives only as the session save/load format, with
the conversion in `grid_session.rs` tested both ways (single, right, down, nested, stack). Fixed on
the way: closing a stacked pane left the focus on the closed pane, because `pop_focused_stack`
mutated the cell's payload before `focused_id()` looked the cell up by the pane it no longer
contained. 19/19 tests pass, `ghost-iced` compiles warning-free.

**M5a part 2 (the pane view)** — the pane layer view is now fully iced-native:
- `panes_grid.rs`: borrows `tab.grid` directly, iterating over cells and building `pane_grid::PaneGrid`
  without an intermediate cache.
- Title bar with 16px label band when frames are enabled (`theme.frame`/`theme.frame_active` outline,
  `theme.tab_bg` text), stack count indication `[active/total]`.
- 4px gutter, `.on_click` wired to focus active `PaneId`, `.on_resize`/`.on_drag` wired to `GridResize`/`GridDrag`.
- Legacy multi-pane layer from `TerminalCanvas` and `GHOST_ICED_PANEGRID` knob deleted. `TerminalCanvas`
  reduced to background/chrome rendering layered above the grid.
- Dynamic sizing from iced layout: canvas records bounds drawn with, update path converts bounds to
  cols/rows via `bounds_to_cols_rows` and calls `resize(cols, rows, cell)`. `AppState::pane_rects()` deleted.
- Subpixel offset origin rounding: canvas paints each pane from rounded origin `(bounds.x.round(), bounds.y.round())`
  by passing subpixel offset `Point::new(bounds.x.round() - bounds.x, bounds.y.round() - bounds.y)` into
  `paint_pane` to eliminate subpixel glyph blur.
- Mouse wheel & key scrolling fixed: normalized Wayland winit pixel scroll delta so discrete ~15px clicks
  never truncate to 0, added `cursor.position_over(bounds)` hit testing per pane, and added `Shift+Up`/`Down`
  line scrolling in Normal mode alongside `Shift+PageUp`/`PageDown` page scrolling.
- Pure-function and integration unit tests: `bounds_to_cols_rows` (normal, fractional `556.80005`, degenerate/too-small),
  stack label string formatting, click→focused-pane mapping, and live scrollback viewport movement under keys. All 22/22 tests pass.
- Dark pane background quirk resolved: `paint_pane` now receives `&self.theme` and renders the default
  terminal background using `rgb(theme.bg)` (`#1a1b26`), matching the container style and eliminating
  the discrepancy where newly spawned panes showed pitch-black (`#000000`) backgrounds. `apply_theme`
  now calls `set_default_fg_color` and `set_default_bg_color` as independent operations, and `AppState::apply_terminal_colors()`
  re-syncs all live panes on split.
- Dynamic process pane & tab titles implemented:
  - `Pane::foreground_process()` queries `nix::unistd::tcgetpgrp` on the pty master fd and extracts the
    foreground command name from `/proc/<pid>/comm`.
  - Distinguishes active commands (`vim`, `cargo`, `htop`, `python`, etc.) from the interactive login shell,
    cleanly displaying the command name when executing and directory when idling at prompt.
  - `Pane::current_dir()` detects working directory via OSC 7 or `/proc/<pid>/cwd` and condenses `$HOME` to `~`.
  - Pane title band (`cell_label`), tab bar label (`Tab::label`), and window title (`App::title`) all dynamically
    update to reflect the active command and directory.
  - All 23/23 unit and integration tests pass.
- `ghost-iced` compiles completely warning-free on both debug and release profiles.

**Next**: Local terminal features parity — M6 (selection, copy/paste, clipboard), search overlay / search mode, and command palette. (Sessions & SSH held for later per user preference).

### Idle GPU, part 2 (branch `perf/idle-gpu-2`) — idle frame tier 30 → 8 FPS
User: "gpu usage is pretty high even when idle still" after the earlier adaptive
pacing fix (6b26de4). Re-measured from scratch on the live instance (started a day
earlier, so the pacing commit IS in the running binary):
- **Pacing works** — replicas built from the real session state (isolated
  `XDG_DATA_HOME`, so the user's sessions were never touched) presented 27-30
  frames/s idle (counted wayland `sendmsg` under strace); the live app matched a
  30 FPS replica on `/proc/PID/io` wchar (1447 vs 1442 B/s ⇒ same present rate).
  Not stuck at 120 FPS, not flood mode, not a visualizer pane (screenshot: 3 text
  panes), not content density (the live screen had FEWER painted pixels than a
  replica replaying its own logs).
- **What it costs**: app's own DRM engine time 14% at 1133x1382 and 30% at
  2280x1382 (scales with window AREA ⇒ full-window redraw bound); system
  `gpu_busy_percent` 50-63% idle vs 1-3% with the process SIGSTOPed.
- **Root cause**: macroquad/miniquad cannot skip a present, and the EGL backbuffer
  is not retained on Wayland (no buffer age), so the whole window is redrawn every
  iteration. Idle cost = fps x full redraw, plus a full-screen compositor
  recomposite for every presented frame. Ghostty (vendored source: `Surface.zig`
  renderer_thread + mailbox/wakeup) draws only when something changed → 0 idle
  presents; we can't match that without a swap-skipping path in miniquad.
- **FIX**: idle tier in the `main()` pacing — 120 FPS while active (1s window),
  8 FPS (125ms) once nothing has changed; the pty-flood no-sleep path is
  unchanged. Measured on the same 3-pane fixture/window: presents 27-30/s → 8/s
  (wayland `sendmsg` count under strace), app engine time 3-4% → <0.5%, CPU
  17% → 6%; system `gpu_busy_percent` with a second 8 FPS client running
  alongside the long-running instance: ~40% → 22-30% (the compositor share is the
  bigger half). Cost: the first keystroke after idling can wait up to one idle
  frame (125ms).
  NOTE: an earlier probe of the live app's rate via `/proc/PID/io` wchar was
  invalid — that stream is dominated by the 2-second session-JSON save
  (~1.4 KB/s), so it did not measure frames. The controlled replicas are the
  only trustworthy rate numbers.
- **DEFERRED (phase 2)**: app-owned offscreen `render_target` + dirty-row
  rendering, blitted once per presented frame — removes per-frame glyph/FFI cost
  rather than the present rate, so it complements this tier instead of replacing it.

### Idle GPU, part 3 (same branch) — output ceiling + 60 FPS active tier
- **Idle tier VERIFIED on the user's own instance**: idle GPU 1-2% (was 50-63%
  system `gpu_busy_percent`, 14-30% app engine time at the same window).
- **Typing was still ~15%**: a keystroke's echo arrived as pty output, and the
  output path was UNCAPPED (no sleep at all while data drained), so a one-cell
  change drove hundreds of full-window presents per second.
- **Change**: `OUTPUT_FRAME_TIME` 8.333ms (120 FPS ceiling while output drains,
  was uncapped), `ACTIVE_FRAME_TIME` 16.667ms (60 FPS, was 120), idle unchanged
  at 125ms. The pacing now always sleeps to its target — the special-cased
  "no sleep during pty activity" branch is gone.
- **NOT verified by the agent: the flood number.** The 1.43s
  `time seq 1 2000000` result was measured with the uncapped output path; with a
  120 FPS ceiling the drain rate is now loop-rate bound. If the re-run regresses,
  revert ONLY the output ceiling and keep the 60 FPS tier.
- Agent-side attempts to automate that benchmark (fixture session in /tmp/gwbench
  with a pane running `seq 1 2000000; echo MARKER-DONE`) proved unreliable: the
  app's async pane-log flush is cut short when the last pane exits (marker never
  lands in the log), and launching replicas needs the project shell's
  `LD_LIBRARY_PATH` or miniquad panics with "Wayland backend failed". Left to the
  user's own in-terminal test.

### Audit fix pass — scrollback byte budget, kitty texture cache, search columns, pty reap, ghostd fd hygiene
- **SCROLLBACK (BYTE BUDGET)**: libghostty-vt measures `max_scrollback` in
  **bytes**, but `Config.scrollback` is a line count — history was silently
  capped at a few KB. `TermPane::new` now converts lines → bytes
  (`lines * 1024`, min 16 MB); values ≥ 1,000,000 are passed through as bytes.
- **KITTY GRAPHICS TEXTURE CACHE**: decoded image textures are cached by
  `image_id` (invalidated when width/height/data-len change, pruned past 64
  entries) instead of re-uploading one `Texture2D` per image per frame.
- **SEARCH COLUMN MAPPING**: matching is now char-based with a per-char column
  map; the old code indexed a per-cell column table with a text offset, so
  multibyte / multi-grapheme cells highlighted the wrong columns. Whole-word
  now uses `is_alphanumeric() || '_'`.
- **SCROLL DIRECTION**: `scroll_page` computed `rows * pages` with `.max(1)`,
  which could flip/zero the direction; fixed, and `scroll_half_page` added —
  `Ctrl+u` / `Ctrl+d` now scroll half a page instead of a full page.
- **PTY REAP**: `poll_reap` only reaped `Child::Exited`; it now reaps `Active`
  children too and stores the real `WaitStatus`.
- **GHOSTD FD HYGIENE**: the `dup`'d pty write fd is now an `OwnedFd` (it was
  never closed — one leaked fd per client attach) and `dup` failure is handled.
- **APP**: new `Action::Backspace` edits the search query / rename buffer
  (previously a `"backspace"` toast); `Mode::Locked` no longer swallows
  pane-mode entry binds; overlay active-pane index clamped to
  `panes.len() - 1`; visualizer frame time clamped to 1–100 ms and the wave
  decay/speed made time-based.
- **AI PANE**: `max_scroll` tracked so `scroll_lines` keeps its position when
  pinned to the bottom (used to snap to 0); prompt no longer pushed twice into
  the request history.
- **CLIPBOARD MIME**: OSC clipboard read now recognizes
  `application/x-uri-list` (typo'd `x-uripboard`); middle-click primary
  selection no longer also overwrites the regular clipboard.
- **TESTS**: 8 new — ai pane send/scroll, layout nested-split collapse, app
  layout remap + locked-mode passthrough, pty reap exit code, term scroll
  direction and scrollback byte conversion.

### 0.1.4 — Polish & Test Audit (test/test-audit-0.1.4)
- **KITTY GRAPHICS SCISSOR & CLIPPING**:
  - Implemented `set_scissor` / `reset_scissor` in `src/main.rs` with `macroquad::window::screen_dpi_scale()` and OpenGL vertical axis inversion (`sh - (y + h)`), ensuring Kitty graphics and terminal contents clip to pane boundaries.
- **PREVIEW PANE ZOOM & PAN**:
  - Added zoom and pan controls to `PreviewPane` in `src/preview.rs` (`zoom_in()`, `zoom_out()`, `reset_zoom()`, `pan()`).
  - Wired mouse wheel zoom and horizontal panning in `src/app.rs` when focused on image preview panes.
- **CONFIGURABLE CURSOR STYLE**:
  - Added `cursor_style` option to `Config` in `src/config.rs` (`"block"`, `"bar"`, `"beam"`, `"underline"`, `"hollow"`).
  - Added `TermPane::parse_cursor_style` in `src/term.rs` and wired it into `apply_terminal_colors` in `src/app.rs`.
- **COMPREHENSIVE TEST AUDIT (62/62 TESTS PASSING)**:
  - Added full test suites across `config.rs`, `keymap.rs`, `palette.rs`, `preview.rs`, `search.rs`, `session.rs`, `term.rs`, `theme.rs`, and `tests/term_core.rs`.

### agy branch — Fullscreen keybind + Strict Locked mode + Middle-click paste + Devenv cached setup
- **WINDOW-SCOPED FULLSCREEN KEYBIND**:
  - `Alt+f` (global) and `Ctrl+p z` (pane mode) added for Zellij-style pane zoom/fullscreen inside the application window (fills content area, does not toggle OS monitor fullscreen).
  - Palette item updated: `"Fullscreen Pane (Window)"`.
- **STRICT LOCKED MODE FIX**:
  - Fixed `Mode::Locked` in `app.rs` & `keymap.rs`. When locked (`Ctrl+g`), **only** `Ctrl+g` unlocks to Normal mode. All other key combinations, releases, repeats, and app shortcuts (`Ctrl+p`, `Ctrl+w`, `Ctrl+d`, etc.) pass directly to the terminal application (full compatibility with `vim`, `micro`, `tmux`, `htop`, etc.).
- **MIDDLE-CLICK PRIMARY PASTE**:
  - `read_primary_text()` added in `clipboard.rs` (`wl-paste --primary`, `xclip -primary`, `xsel --primary`).
  - Middle mouse click in `app.rs` sends mouse event if tracking is enabled in the terminal, or pastes the primary selection when tracking is inactive.
- **DEVENV FAST CACHED SETUP & RUST-OVERLAY**:
  - Configured `devenv.yaml` with `rust-overlay` (`github:oxalica/rust-overlay`) and `devenv-nixpkgs/rolling` matching the local Nix store cache from `halloy`.
  - Updated `devenv.nix` with declarative `languages.rust` components (`rustc`, `cargo`, `clippy`, `rustfmt`, `rust-analyzer`), reused cached GUI/graphics packages (`mesa`, `libGL`, `wayland`, `libxkbcommon`, `libxcb`), X11 libraries, and dynamic `env.LD_LIBRARY_PATH` via `lib.makeLibraryPath`.
  - Added `.envrc` for direnv integration and updated `.gitignore` for `.devenv`, `.direnv`, and `ghostd/.toolchain`.


### 0.1.3 — ds sessions own session-manager entry + reliable kill (feat/ds-session-kill)
- `ds <host>` now creates/switches to its OWN local session `ssh-<host>`
  (was: dumped a tab into the current session, invisible in the manager).
  Repeat `ds <host>` focuses/reconnects the existing attach pane instead
  of stacking duplicates (ghostd serves one client at a time anyway).
- Session manager `k` = kill the selected session's remote daemons over a
  FRESH ssh (`ghostd kill <session>` + `pkill -9` fallback) — works even
  when the local pane is dead/stuck. Pure ds sessions (`ssh-<host>`) are
  removed from the list after the kill; mixed sessions keep their local
  tabs. Hint line updated: `k kill remote`.
- ghostd hardened for stuck daemons:
  - spawn_child returns the pty child's pid; pidfile now two lines
    (daemon pid + child pid).
  - `ghostd kill` escalates: SIGTERM daemon → 3s → SIGKILL, then kills the
    child's process group (TERM → KILL). Old behavior gave up after 5s and
    removed the pidfile while a wedged daemon lived on, with no pidfile
    left to ever find it again.
  - FIXED BUG: daemon cleanup used `kill(-1, SIGTERM)` — a broadcast to
    EVERY process the user owns on the remote host. Now kills only the
    session's process group.
  - child is a session leader (forkpty login_tty), so group-kill by
    -child_pid reaches the shell AND its children (sleep 300, builds…).
- App + ghostd bumped 0.1.2 → 0.1.3 — the remote version check
  (`ghostd <ver>`) forces re-upload of the hardened daemon.
- REQUIRES `bash scripts/build-ghostd.sh` before testing (assets still
  0.1.2 until the embedded binaries are rebuilt).
- ROFI LAUNCH FIX (outside repo): ~/.local/bin/ghostwave-launcher — NixOS
  has no loader cache and rofi/hyprland sessions don't carry
  LD_LIBRARY_PATH, so miniquad's runtime dlopen of wayland/EGL/X11 died
  silently ("Wayland backend failed") when launched from a menu. Wrapper
  discovers store lib dirs (maxdepth 3, package-name-scoped to dodge
  lmstudio/wine bundles) and execs. Desktop entry points at the wrapper.

### Recent work (commits 2571ca2 → HEAD)
- Render: always-redraw per libghostty-vt docs (ghostling-rs pattern);
  pane bg = terminal default from snapshot.colors(). Fixed blank/flicker.
- Mouse: drag selection only while left button held; click focuses pane.
- SELECTION OFFSET FIX: SelectionExt::point() subtracted rect.x/y but
  app.rs already passes pane-local coords → double-subtract put the
  highlight ~1.2 rows ABOVE the mouse (pane y=32, cell_h=26). Removed.
- CLIPBOARD FREEZE FIX: clipboard tools were waited on synchronously on
  the main thread. wl-copy can stay alive as clipboard daemon (freeze on
  Ctrl+Shift+C), wl-paste blocks forever on an empty clipboard (freeze on
  Ctrl+Shift+V). Now: children reaped on background threads, paste reads
  have a 1s timeout.
- BACKEND: forced LinuxBackend::WaylandOnly — the WaylandWithX11Fallback
  build silently ran on XWayland (workspace switches → Hyprland "not
  responding" popup when the X11 loop blocked). Wayland-only handles
  workspace switches cleanly. (Panics on X11-only sessions; fine here.)
- Removed per-frame "pane rect" debug spam from layout_all.
- Cursor blink (500ms phase) driven by terminal blink mode.
- NO-FREEZE ON HIDDEN WORKSPACES (Hyprland "not responding"): root cause —
  with vsync (eglSwapInterval 1), eglSwapBuffers blocks until the next
  wl_surface.frame callback; Hyprland stops sending those when the window
  is on another workspace → loop stalls forever → no wl_display pong →
  "not responding" overlay. Fix: platform swap_interval = Some(0) (swap
  never waits on the compositor) + manual 60fps cap (sleep to 16.6ms per
  iteration) in main() so CPU stays sane. User verified switching
  workspaces leaves the app responsive.
- FLOOD THROUGHPUT: `time seq 1 2000000` was 12.6s vs ghostty 0.56s /
  cosmic 0.63s (blocked pty writes = we drained one 64KB buffer per
  16.6ms frame tick). Fix: event-driven pacing — main loop skips its
  frame sleep while pty data is pending (app.pty_activity), so floods
  process back-to-back. Result: 1.43s (9x faster; remaining gap vs
  ghostty ≈ session-log tee + full-grid redraw per frame).
- RENDER cleanup: blink timer + cursor block moved out of the per-row
  loop (was re-running per row = redundant FFI calls per frame).
- EXPLORED + REVERTED: per-row damage skipping (Dirty::Partial/Clean)
  — the GL backbuffer is NOT guaranteed to retain content between
  swaps on Wayland/EGL (no buffer age), so skipped rows show
  black/garbage (the old flicker bug). Proper damage rendering needs
  an app-owned offscreen render target (macroquad render_target) —
  deferred; idle CPU is already ~0.
- REMOTE DURABLE SESSIONS (waveterm-style) — `feat/remote-durable` branch.
  STATUS: PR #1 MERGED by user to master (d115196); follow-up fixes on the
  branch (7872569) awaiting user re-test; master protected on forgejo, user
  gates all merges ("dont merge till i confirm"):
  - ghostd: standalone pure-Rust daemon (ghostd/ crate, lib+bin).
    Sessions survive ssh drops (daemonize + setsid + SIGHUP-immune); pty
    ALWAYS drained to a front-truncated ring log so children never stall
    while detached and drop-gap output replays on attach; first-attach
    timeout (default 300s) kills orphans; log cap (32MB) bounds disk;
    new/attach/list/kill/kill-all; DCS-based mid-session resize.
  - Build: static musl binaries COMMITTED under assets/ (x86_64 + aarch64,
    built with official rustc + rust-std components + rust-lld — no zig, no
    C toolchain, no nix build needed); scripts/build-ghostd.sh rebuilds
    them with plain cargo on any distro (rustup musl targets), NixOS
    fallback path included. Flake REMOVED (was bootstrapping a musl gcc
    toolchain from source for a zero-C crate — overkill).
  - Client: `ds <host>` palette entry → ssh -t + ghostd attach; binary
    bootstrapped over ssh (uname -m → version check → stdin upload,
    embedded static musl binaries); restore shows remote panes
    DISCONNECTED (Enter to connect, never auto-connect on start); bounded
    auto-reconnect (5 attempts, 3s→15s backoff → manual); `k` kills the
    remote session; session manager shows an "ssh" marker; [remote] config.
  - FIXED (7872569): shell-quote escaping emitted literal backslashes
    (remote checks always failed → stale loop); `exec attach || new`
    fallback was dead code (exec replaces the shell → sessions never
    created). Verified end-to-end on orac: session created, daemon
    persists. Kill message now accurate ('no ghostd binary' only when
    truly missing).
  - NEW: plain `ssh <host>` palette connections live in their OWN named
    session (ssh-<host>) in the session list — no durable marker, resume
    by re-running the command.
  - FIXED (3363fff): ds pane input — the ssh -t remote pty was canonical+
    echo, so Ctrl+C SIGINT'd the attach process (dropped the connection)
    and arrows echoed as ^[[A. cmd_attach now cfmakeraw()s its stdin:
    every byte (0x03, arrows) flows to the daemon's child pty, where the
    shell's own signal handling applies. Static musl binaries rebuilt.
  - FIXED (uncommitted, ghostd 0.1.1 + app 0.1.1): fish-shell hosts (homelab)
    rejected every remote command — ssh runs the command via the login
    shell, and fish refuses `BIN="..."` assignment ("Unsupported use of
    '='"). All four remote command sites wrapped in `sh -c '...'` so POSIX
    sh parses them regardless of login shell: remote_attach_cmd
    (attach/fallback chain), kill_remote_pane, upload_ghostd version
    check + upload. Dropped the inner single quotes in the kill message
    (would have terminated the sh -c wrapper). VERIFIED on homelab (fish):
    full ds chain runs, session created, fish welcome replayed.
  - FIXED (same commit): `ds` hung with ZERO output on both hosts even
    after the fish fix — root cause chain, all addressed:
    a. ghostd handle_client read the SIZE handshake NONBLOCKING and bailed
       on EAGAIN → thread died silently → attach connected but nobody ever
       serviced it → hang with no output. Now polls (5s bound) for the
       line.
    b. New OK-ack protocol: daemon acks after SIZE; attach waits ≤8s for
       the ack then exits 1 ("session 'x' not responding — recovering")
       so the `|| new` fallback fires. `new` probes first (SIZE handshake,
       2s bound): live daemon → "already exists"; unresponsive daemon →
       SIGTERM → SIGKILL → remove state → rebind fresh.
    c. attach ignored stdin/stream EOF (spin at 100% CPU forever holding
       the daemon's client slot — the zombie attaches found on orac).
       Now exits on both.
    d. attach sends SIZE 80x24 fallback when stdin isn't a tty (was:
       no SIZE → daemon's 5s wait → dead thread).
    e. upload writes ghostd-<arch>.new + mv (was: "Text file busy" when a
       running daemon holds the binary path).
    VERIFIED end-to-end: orac SIGSTOP'd daemon → chain attach-timeout →
    probe → SIGTERM/SIGKILL → fresh daemon → re-attach → live bash
    prompt; homelab fish full chain; no zombie attaches after pane close.
  - DEBUG: GHOSTWAVE_DEBUG=1 (app, stdout traces: ssh argv, upload steps,
    reconnect, kill) and GHOSTD_DEBUG=1 (remote: eprintln + append to
    ~/.local/share/ghostwave/debug.log — daemon/attach/new/probe/recover
    traces). Off by default.
  - Versions bumped 0.1.0 → 0.1.1 (BOTH crates — the ssh bootstrap
    compares `ghostd --version` against the app's CARGO_PKG_VERSION, so a
    ghostd-only bump would never re-upload). Musl assets rebuilt both
    arches.
  - OSC 52 clipboard over ssh: already wired (on_clipboard_write → wl-copy).
  - Wrap/reflow semantics pinned in tests/term_core.rs — both work in
    libghostty-vt 0.2.1; the "cut off" report was a TUI clipping its own
    panels, not a terminal bug.
- LIVE SESSIONS (zellij-style background sessions) — `feat/live-sessions`:
  - Switching sessions no longer kills the outgoing session's processes.
    App.parked (Vec<LiveSession>{name, tabs, active_tab}) parks the outgoing
    session's LIVE panes instead of dropping them (drop → Pty::Child::Drop →
    SIGHUP was the old kill); switching back swaps the live tabs in —
    instant, no shell respawn, no log replay, process state intact.
  - Drain loop extracted (drain_tab_panes) and run over active AND parked
    sessions every frame: background children never stall on full pty
    buffers, and their durable logs keep growing while hidden (poll_pty tees
    to the log regardless of render state).
  - Remote reconnect pass extracted (remote_reconnect_pass) and run over
    parked sessions too — a ghostd session keeps auto-reconnecting while you
    work elsewhere; respawn_waiting/set_remote_manual cover parked panes.
  - Quit persists ALL live sessions (save_all_sessions) — parked metadata
    can't be lost (logs were already fresh from background draining).
  - Manager `d` delete also drops a parked session (kills its processes —
    the point of deleting). Delete-CURRENT-session drops instead of parking
    so a deleted session can't resurrect via the continuous save.
  - Rename now targets the SELECTED session (pre-existing bug: it renamed
    the current session no matter what you picked) and renames the parked
    in-memory entry too.
  - Empty sessions are not parked (skips the disk write — a deleted or
    never-populated session can't come back).
  - Tradeoff: parked sessions keep terminal grids resident in memory (fine
    on 32GB; could cap "keep last N alive" later).
- FLOATING PANES (zellij) — same branch:
  - Ctrl+p f = toggle float: the focused tree pane leaves the layout and
    floats above it (same size/position); f again embeds it back at its
    EXACT original tree spot (layout.rs FloatOrigin records the split
    parent/side/dir/ratio before the tree collapses — round-trips cleanly,
    stacks included; stale origins fall back to a split next to the active
    pane).
  - Ctrl+h (Move) moves the float by cell steps; Ctrl+n (Resize) resizes
    it; both clamp to the content area. Move mode is now STICKY (zellij
    behavior) — removed Move from the one-shot modal auto-exit; Esc or
    Ctrl+h exits. Unbound keys in modal modes are ignored, so nothing leaks
    into the terminal while moving.
  - Mouse: floats hit-test BEFORE tree panes (topmost, z-order = list
    order); clicking focuses. Floats render last (on top), with "float N"
    frames; the ACTIVE pane's frame is now 2px (inactive 1px) so focus is
    obvious.
  - Hidden BG PANES: pane-mode `e` hides the active float (pane-mode `E`
    cycles floats and shows them). Hidden floats are not rendered or
    clickable but keep RUNNING and draining — the "shunt to bg" case:
    float a build, hide it, keep working; E brings it back.
  - Guardrails: zoom on a float toasts; focus/cycle from a float re-enters
    the tree; close (x) closes a float; splitting with an empty tree (only
    pane floating) works; the usize::MAX empty-tree sentinel is invisible
    to leaves()/layout()/path_to_pane (no render panic).
  - Persistence: TabState.floating (serde default → old files fine) stores
    pane id (remapped), rect in CELLS (window-size independent), origin,
    hidden flag; restored on session load/switch.
  - Unit-tested: 6 layout tests (float/embed round-trips: split, root
    stack, root leaf, stack-under-split, stale-origin rejection, sentinel
    invisibility).
- FISH 4 STARTUP ESCAPE GARBAGE FIX (`^[[?0u^[P>|ghostwave^[\…`):
  - Symptom: at app start / new panes, caret-escaped terminal responses
    (kitty `CSI ? 0 u`, `DCS >|ghostwave ST`, OSC 11 `rgb:…`, DA
    `CSI ? 62;1;6;22c`) printed after the prompt. Decoded: they are the
    terminal's ANSWERS to fish 4.x startup probes — kitty keyboard query
    `CSI ? u`, XTVERSION `CSI > q`, DA `CSI c`, OSC 11 bg query (fish's
    binary has query_kitty_progressive_enhancements).
  - Root cause: the restore path spawned the shell BEFORE replaying the
    session log — the probes sat in the pty buffer during the replay and
    were answered AFTER fish's read timeout; fish's line editor then
    echoed the stale responses as caret-escaped garbage. Running `fish`
    later was clean because the frame loop drains promptly.
  - Fix: (1) tabs_from_session now creates the terminal WITHOUT spawning,
    replays history, THEN starts the shell; (2) new prime_pty() drains the
    child's initial burst synchronously right after spawn (250ms cap, 60ms
    idle break) so probes are answered inside fish's read window; spawn
    refactored into start_child (unprimed — reconnect must not block the
    main loop) + spawn_child (primed, used by restore); respawn() reuses
    start_child and now also works for program=None ($SHELL) panes.
  - Verified: isolation test proves the terminal core answers all four
    probes with the correct query forms (OSC 11 only after a color was
    set — ghostty behavior, confirmed in stream_terminal.zig); prime test
    proves early child output is processed during construction.
    User-verified: garbage gone at app start.
- Ctrl+Tab = next tab, Ctrl+Shift+Tab = previous tab (global binds).
- PIN MARKER: drawn with primitives (no 📌 in the font stack → tofu).
- FONT STACK: JetBrains Nerd variants (6 family NAMES, same design)
  collapsed to one slot → Noto Sans Symbols 2 / Symbols / Liberation
  Mono now load; emoji in terminal output render via fallback.
- DURABLE SESSIONS (zellij-style): every pane's pty output tee'd to
  ~/.local/share/ghostwave/panes/ (cap: config max_log_mb, default 32 MiB,
  front-truncated LIVE during logging; prune of dead logs at save).
  NAMED sessions under sessions/<name>.json: Ctrl+o → session mode →
  w = session manager (↑/↓ + Enter switch, n new, r rename, d delete),
  d = detach (save + quit), Esc exit. Startup resumes the last-used
  session; legacy session.json auto-migrates to sessions/default.json.
  THE WHOLE SESSION IS SAVED/RESTORED (all tabs + all terminal panes,
  full scrollback via log replay, AND the split/stack layout tree —
  TabState.root carries layout::Node, ids remapped at save time through
  the kept-pane mapping; old files without root fall back to one pane) —
  pin concept REMOVED (it only ever
  filtered restore, causing "only the pinned tab came back"); right-click
  tab menu is now rename/close only. Saved CONTINUOUSLY (every 2s) so a
  kill/crash loses at most seconds. Preview/AI panes not restored.
  Unit-tested (save/load/delete/rename, front truncation).
  NOTE: each restore spawns a fresh shell whose greeting appends to the
  log — shell greetings accumulate across restarts (cosmetic).
  TODO (waveterm "durable"): remote-TTY rejoin (attach to a remote
  session like tmux/zellij) — deferred until local sessions are solid.
- REMOVED dead Ctrl+Shift+A bind + ToggleAi action + ai_visible flag
  (never wired to anything; AI pane reachable only via palette
  "New AI Chat" and is untested/not a real feature yet).
- Esc exits all modal modes; rename/search Enter+Esc actually work now.
- Theme → terminal fg/bg wired (set_default_fg/bg_color).
- Tab label follows active pane OSC title (fish emits none → stays "Tab 1").
- Search matches get solid bg highlight.
- Key releases only forwarded for unbound keys (no spam to apps).
- Zoomed pane re-grids to full content rect.
- FONT SIZE FIX: font_px was multiplied by screen_dpi_scale() (wl buffer
  scale = 2.0) but macroquad's draw_text_ex/measure_text work in LOGICAL
  px (miniquad internal dpi_scale = 1.0 here). Result: font_size=12
  rendered at 24px and the grid halved (75x36). Fixed: font_size is now
  interpreted in POINTS like ghostty/waveterm (12pt → 16px @96dpi, no
  dpi multiplication) → font_px=16, cell=10x26, grid 112x50. Adjustable
  in ~/.config/ghostwave/config.toml (font_size, points). Line wrapping
  at the right edge follows the grid.
- waveterm command blocks: LEGACY feature (new waveterm dropped them) —
  NOT implementing. AI pane context injection: deferred by user.
- Scrollback search: libghostty-vt 0.2.1 has NO scrollback traversal API
  (RowIterator = viewport only, no search API) → search stays viewport-
  scoped. Would need ghostty upgrades or scroll-crawl; deferred.

### Timeline (older)
- build6: ✅ compiles, 0 errors. Binary: target/debug/ghostwave
- run1/run2: `Failed to initialize through wayland! Trying X11 instead` →
  panic `LibraryNotFound(DlOpenError("libX11.so.6"))`. Cause: miniquad
  dlopens GL/wayland/x11 libs at runtime; nix-shell had EMPTY
  LD_LIBRARY_PATH (mkShell does not derive it from buildInputs here).
- FIX (per nix.dev declarative-shell doc + MCP): proper modern names are
  top-level `libx11`, `libxcursor`, `libxrandr`, `libxi`, `libxext`,
  `libxcb` (the `xorg.*` set is deprecated — verified via nixos MCP:
  libx11 1.8.13, libxcursor 1.2.3, ...). `LD_LIBRARY_PATH` set as a
  **direct mkShell attribute**: `LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath
  [libGL wayland libxkbcommon libx11 libxcursor libxrandr libxi libxext
  libxcb]` (nix.dev: any string-coercible attr becomes an env var).
  Verified: LDPATH now lists all 9 lib dirs, no deprecation warnings.
  devenv.nix uses `libs = [...]` (devenv's equivalent mechanism).
  NOTE: must use PROJECT shell.nix (`nix-shell ./shell.nix`); a minimal
  dev shell lacks these libs → runtime dlopen failure.
- run3: ✅ windowing init works now (Wayland path succeeded, no more X11
  fallback) → NEW panic: `src/app.rs:173 index out of bounds (len 0)`.
  Cause: `App::new` called `new_term_pane` (which does
  `tabs[active_tab]`) before any tab existed. FIXED: initial TermPane
  created first, then `tabs: vec![Tab::new(Pane::Term(initial), ...)]`.
- run4+: pane rendering was broken two ways:
  1. Original: `clear_background` + pane bg painted EVERY frame, but
     cells only when the render-state snapshot was dirty → every clean
     frame painted over the text → blank/flicker (text only during
     typing/force-repaint).
  2. Debug detour: added skip-on-clean + TEMP red probe rect + [dbg]
     logging → pane stayed RED (probe drawn every frame over skipped
     cells). Red probe was the "all red" the user saw.
  3. Scissor detour: `quad_gl.scissor` with dpi-scaled coords was
     suspected of clipping pane draws; disabled it as a test. NOT the
     root cause; set_scissor is still active in main.rs but untested
     for correctness after the render rewrite.
- FINAL FIX (commit 2571ca2): follow the documented libghostty-vt
  pattern (ghostling-rs reference + example/c-vt-render): update render
  state + redraw ALL rows/cells EVERY frame, pane bg = terminal default
  (`snapshot.colors().background`, not theme bg), reset dirty flags at
  end. Removed skip_cells, red probe, [dbg] logging. VERIFIED: prompt,
  text output, and block cursor all render and persist at idle.
- Mouse: selection drag now requires left button physically held
  (`ev.button_down == Some(Left)`). ghostty's SelectionGesture keeps
  `left_click_count > 0` after release (for double/triple-click timing —
  see SelectionGesture.zig release() docs), so gating drag on click_count
  alone made any hover after a click extend the selection.

Build log history: /tmp/ghostwave-build{1..6}.log; run logs:
/tmp/ghostwave-run{1..11}.log
- build1: nix feature `pty` doesn't exist → fixed
- build2: zig 0.16.0 rejected → shell.nix now uses `zig_0_15` (0.15.2)
- build3: 52 errors in our crate → fixed in batches (see below)
- build4: 17 errors → fixed (conf path, fd traits, borrows, kitty restructure)
- build5: 2 errors → macroquad 0.4.16 Conf wraps `miniquad::conf::Conf`
- build6: ✅ 0 errors, 17 warnings (dead code only). Binary: target/debug/ghostwave

Key API gotchas discovered (all fixed):
- macroquad 0.4.16: NO public set_scissor_rect (use get_internal_gl().quad_gl
  .scissor, physical px, y-flip internal); Conf = { miniquad_conf, .. };
  no screen_size() (use screen_width/height); Texture2D has no delete().
- nix 0.31: write/dup take AsFd (wrap raw i32 in BorrowedFd);
  Command::arg0/exec need std::os::unix::process::CommandExt.
- libghostty-vt 0.2.1: RustPngDecoder has NO public constructor (stale
  docs) → wrote own DecodePng (term.rs PngDecoder, png crate);
  set_kitty_image_from_temp_file_allowed (not ..._temp_file_...);
  terminal.reset() returns (); CursorVisualStyle::BlockHollow;
  Graphics is not Copy → kitty render is an associated fn taking
  (&Terminal, &mut PlacementIterator, &Graphics).

## TODO next (in order)
1. ✅ DONE: splits/scissor verified live (user tested pane creation; renders fine).
2. ✅ DONE: screenshot QA with grim + vision at every step.
3. ✅ DONE: theme → terminal default fg/bg wired
   (apply_terminal_colors; set_default_fg/bg_color on all panes).
4. ✅ DONE: tab name follows active pane OSC title (unless renamed).
5. ✅ DONE: zellij binds tested live by user (Esc-cancel added on request).
6. ✅ DONE: release build; perf fine (24% CPU idle; always-redraw kept —
   per-row dirty optimization deferred).
7. ✅ DONE: durable sessions (pin via right-click tab, restore with
   scrollback, continuous 2s saves).
9. ✅ DONE: zellij floating panes + hidden bg panes (Ctrl+p f/e/E; Move
   mode sticky; active-pane frame 2px).
10. ✅ DONE: polish queue — middle-click primary paste, kitty graphics scissor/clipping, preview pane zoom/pan, configurable cursor style (block/beam/underline/hollow).
11. ✅ DONE: comprehensive test suite audit (62/62 unit & integration tests passing).

---

## Build / run commands (from project root — project shell.nix included)

    nix-shell ./shell.nix --run "cargo build 2>&1"
    nix-shell ./shell.nix --run "cargo run 2>&1"
    # or interactively:  nix-shell   then   cargo run

- `a full dev shell (cargo/rustc/zig/fontconfig)` was extended with: cargo, rustc 1.95, rustfmt,
  clippy, **zig_0_15 (0.15.2)** (required by libghostty-vt-sys vendored
  build; plain `zig` = 0.16.0 BREAKS ghostty's build), pkg-config,
  fontconfig, jetbrains-mono.
- **Why zig**: build-time only. `libghostty-vt-sys`'s build.rs runs
  `zig build -Demit-lib-vt=true` on a pinned ghostty commit
  (a887df42c = 1.3.2-dev, requires zig 0.15.2 EXACTLY) to produce the
  terminal core lib; it is statically linked. The nixpkgs-installed
  `libghostty-vt.so` (ghostty 1.3.1) only has 43 helper symbols
  (key/OSC/SGR) — NO terminal emulation — so vendored is the only path.
  Our code is 100% Rust.
- scissor: macroquad 0.4.16 has NO public set_scissor_rect. main.rs uses
  `unsafe { get_internal_gl().quad_gl.scissor(Some((x*dpi, y*dpi, w*dpi,
  h*dpi))) }` (GL scissor = physical px, Y flipped internally).

## Architecture (all files written, none compile-verified yet)

```
src/main.rs     macroquad event loop: input poll, mouse, render, chrome
                (tab bar, pane frames, status bar, palette, search/rename
                overlays, toasts). Uses ALL_KEYS table + char queue; key
                repeat synthesized for non-text keys only.
src/app.rs      App: tabs (Vec<Tab>), Tab{root: Node tree, panes: Vec<Pane>,
                active, zoomed}, Mode, theme, palette, actions
                (execute()), layout_all(), mouse hit-testing.
src/term.rs     TermPane: libghostty-vt Terminal<'static,'static> +
                RenderState + Row/Cell iterators + key/mouse encoders +
                selection gestures + pty + kitty graphics + search
                integration. Adapted from ghostling-rs main.rs.
src/pty.rs      forkpty (nix), O_NONBLOCK master, winsize ioctl, child
                reap. Pty::new(size, program, args, cwd) — program=None
                → $SHELL; used for `ssh host` panes too.
src/keymap.rs   zellij mode machine: Normal/Pane/Tab/Resize/Scroll/Search/
                EnterSearch/Move/Session/Locked/RenameTab/RenamePane.
                Mode entry: Ctrl+p/t/n/s/o/h/g; Ctrl+q quit. Waveterm
                global binds (Ctrl+Shift+P/F/C/V/D/W/T/A, Ctrl+D/W/M/K,
                F2, Ctrl+[/], Ctrl+1-9, Ctrl+Shift+1-9/arrows/hjkl,
                Alt+n/h/l/j/k/i/o/=/-). Macroquad KeyCode → ghostty Key
                table + unshifted_codepoint.
src/layout.rs   binary split tree Node{Split|Leaf|Stack} + Path ops:
                split_at, stack_at, close_at (collapse), resize_at,
                swap_panes, cycle_stack, focus_in_direction (rect-based),
                next_leaf, layout()->[(pane,Rect)]. Dir{Left,Right,Up,Down}
                vs SplitDir{Horizontal,Vertical}.
src/search.rs   viewport-scoped search: Match{row,col_start,col_end},
                case/wrap/whole-word toggles, run_on_rows() over a
                RowIteration. Scrollback search = TODO.
src/palette.rs  command palette, dynamic "open <path>"/"ssh <host>"/theme
                entries, fuzzy-ish substring filter.
src/preview.rs  file preview pane: text (wrapped), image (image crate →
                Texture2D, fit-scale), directory listing.
src/ai.rs       Wave AI chat pane: OpenAI-compatible /chat/completions on a
                worker thread (ureq), configurable base_url/model/api_key_env
                (default MiniMax), history + busy indicator.
src/theme.rs    8 themes (tokyonight default, dracula, gruvbox, catppuccin,
                nord, solarized, monokai, ghostty). Theme = chrome colors;
                terminal content colors come from libghostty's built-in
                palette (set_default_fg/bg NOT yet wired).
src/config.rs   ~/.config/ghostwave/config.toml (font_size, theme,
                scrollback, padding, row_gap, shell, [ai]). Default file
                written on first run by main.rs.
src/clipboard.rs wl-copy/wl-paste/xclip/xsel, no deps.
assets/JetBrainsMono-Medium.ttf  (from libghostty-rs example/fonts)
```

## Key API facts (from reading libghostty-vt 0.2.1 source — trust these)

- Terminal::new(TerminalOptions{cols,rows,max_scrollback}) → Terminal<'static,'static>
  (callbacks must be 'static; capture raw fds / Arc, never &self).
- Effects: on_pty_write(|t,data|), on_bell, on_enquiry, on_xtversion,
  on_title_changed, on_pwd_changed, on_size (→SizeReportSize),
  on_color_scheme, on_device_attributes, on_clipboard_write
  (ClipboardWrite{location(), contents()→ClipboardContent{mime,data}}).
- Kitty: set_png_decoder(RustPngDecoder::new()) global once; terminal
  set_kitty_image_storage_limit / from_file / from_temp_file /
  from_shared_mem; render via PlacementIterator::update(&Graphics) →
  set_layer(Layer) → placement.placement_render_info(&image,&term).
- Render: RenderState::update(&term) → Snapshot{dirty(), colors()
  {foreground,background,cursor,palette[256]}, cursor_visible,
  cursor_viewport, cursor_visual_style, set_dirty(Clean)}.
  RowIterator::update(&snapshot) → next()→&RowIteration{dirty(),set_dirty};
  CellIterator::update(row) → next()→&CellIteration{graphemes_len,
  graphemes_utf8(&mut String), fg_color, bg_color, is_selected,
  has_styling, style{inverse,bold,...}}.
- Key: Key enum = winit-style names (maps 1:1 to macroquad KeyCode).
  Encoder::set_options_from_terminal + Event::set_action/key/mods/
  consumed_mods/unshifted_codepoint/utf8 → encode_to_vec.
- Mouse: Encoder::set_size(EncoderSize{screen_*,cell_*,padding_*}) +
  set_any_button_pressed + set_track_last_cell; Event action/button/position.
- Selection: gesture::{Gesture,PressEvent,ReleaseEvent,DragEvent} +
  Geometry; terminal.set_selection; copy via
  format_selection_alloc(None, FormatOptions::new()) (active sel) or
  with_selection(&sel) from select_all().
- Scroll: terminal.scroll_viewport(Top/Bottom/Delta(isize)/Row(usize));
  scrollbar() → {total:u64, offset:u64, len:u64}.
- !Send/!Sync: all terminal ops on the main thread. PTY reads happen
  non-blocking in the main loop (ghostling pattern), no reader threads.
- ScrollViewport note: mouse wheel → scroll_viewport(Delta((dy*-2.5) as isize)).

## Zellij binds implemented (from default.kdl, verbatim where possible)

- Ctrl+p PANE: n new(down), d down, r right, s stacked, x close,
  h/j/k/l+arrows focus, p cycle, f float, e hide float, E cycle float,
  z frames, w cycle-stack, c rename
- Ctrl+t TAB: n new, x close, r rename, h/l prev/next, b break-pane,
  1-9 goto, Tab last, s/[ /] toast
- Ctrl+n RESIZE: h/j/k/l grow, H/J/K/L shrink, =/+ grow-all, - shrink-all
- Ctrl+s SCROLL: j/k 1 line, d/u half page, Ctrl+f/Ctrl+b pg, Ctrl+c bottom,
  s enter-search, e toast
- SEARCH: n/p next/prev, c case, w wrap, o whole-word; EnterSearch types
- Ctrl+h MOVE: h/j/k/l move pane, n next, p prev
- Ctrl+o SESSION: all toasts (durable sessions deferred)
- Ctrl+g LOCKED: passthrough, Ctrl+g unlock
- Ctrl+q quit; Ctrl+Shift+P palette (waveterm)

## Waveterm features ported

tabs, split panes (right/down/stacked), zoom (Ctrl+M), command palette
(Ctrl+Shift+P; open <path>, ssh <host>, theme <name>), find in terminal
(Ctrl+Shift+F, viewport-only), copy (Ctrl+Shift+C, sel-or-screen),
paste (Ctrl+Shift+V), clear (Ctrl+K), file preview pane, AI chat pane
(Ctrl+Shift+A), SSH connect, themes, pane frames + status bar hints.
NOT ported: durable sessions, web preview panes, wsh, workspaces, command
blocks with exit-code headers, block numbers overlay (Ctrl+Shift held),
remote connection switcher (Ctrl+G — clashes with zellij locked).

## KNOWN BUGS / TODOS (fix in order)

1. [BLOCKER] main.rs `set_scissor_rect` doesn't exist in macroquad 0.4.16.
   Fix: replace with quad_gl.scissor via get_internal_gl (see STATUS).
2. [BLOCKER] compile-verify everything — zero successful builds yet.
   Expect: lifetime errors on Terminal<'static,'static>, handler macro
   closure signatures, DrawTextureParams Rect type, etc.
3. term.rs: render() force_repaint logic — pane redraw when selection
   changes relies on force_repaint being set by input handlers (done) but
   scroll bar & exited overlay draw even when skip_cells — verify.
4. search: matches highlight uses search_match color for fg only; bg
   highlight not drawn (inverse would be nicer).
5. tab name: not set from shell title (OSC 0) — tab.name stays "Tab N";
   pane frame label uses terminal title already.
6. theme doesn't apply terminal default fg/bg (set_default_fg_color /
   set_default_bg_color exist on Terminal — wire in apply_theme).
7. handle_key release path sends releases for ALL keys incl. bound ones.
8. Ctrl+Shift+1-9 block-number focus maps to pane index — fine.
9. Zoom: other panes' pty output still processed (good); layout_all skips
   zoomed panes' set_grid (grid stays stale after unzoom until resize —
   verify).
10. AI pane: no streaming, no context injection from terminal output.
11. Image preview: no zoom/pan; kitty graphics images rendered unclipped.
12. Repeat synth only for non-text keys — text repeats rely on platform
    char events (macroquad repeats get_char_pressed — believed OK).

## Reference files (in /tmp/ghost-research — may be wiped on reboot)

- ghostling-rs-main.rs — the base we adapted (1405 lines)
- ghostling-main.c — C API reference (ghostty-org/ghostling)
- libghostty.h / ghostty.h — C headers
- vt/libghostty-vt-0.2.1/ — full safe crate source (API truth)
- vt-sys/libghostty-vt-sys-0.2.1/ — FFI + build.rs (zig vendored logic)
- waveterm-keybindings.mdx — waveterm's real keybind table
- zellij default.kdl (fetched from zellij-org/zellij main)
- ghostty pinned commit a887df42 (1.3.2-dev) — vendored by build.rs

## MAYBES / FUTURE EXPLORATION

- **Long-Running Command Notifications (Foot / WezTerm style)**:
  - System desktop notification via `notify-send` / FreeDesktop D-Bus when a background command takes >5s and finishes while unfocused (`cargo build`, `nix build`, `git clone`).
- **Scrollback Error Minimap Track (Wave / Warp style)**:
  - 4px right-side gutter rendering color tick marks (red for non-zero exit codes/compiler errors, yellow for search hits, blue for prompts) with click-to-jump.
- **Quick-Select Regex Tagging Mode (WezTerm / Contour style)**:
  - Fast keyboard picker overlaying 2-letter tags (`[aa]`, `[ab]`) over Git commit hashes, IPs, URLs, Docker IDs, UUIDs to copy or open without mouse.
- **Remote SSH SFTP / File Tree Drawer (Tabby / Termius style)**:
  - Slide-out file tree for durable SSH sessions (`ghostd`), drag-and-drop auto-upload/download, open remote files in local `PreviewPane`.
- **Smart Semantic Double-Click (Contour style)**:
  - Context-aware selection expanding over `file.rs:120:5`, inner quoted strings, and JSON key-values on double click.
- **Subtle Cursor Micro-Glide Animation (Neovide / Rio style)**:
  - Smooth spring interpolation (20-40ms) on cursor jump.
- **Live Status Sparklines**:
  - Live 1-char CPU / memory / git branch status widgets in status bar.

## External facts

- Target platform: Linux/Wayland (Hyprland dev machine), wl-copy/wl-paste
  clipboard, fish shell dev environment
  Wayland session (WAYLAND_DISPLAY=wayland-1). wl-copy/wl-paste present.
- nixpkgs zig attr currently resolves to 0.16.0 (ghostty needs ≥0.15.2).
- rustc 1.95.0 in nixpkgs (crate needs ≥1.90).
- To screenshot for QA: grim/hyprctl may exist — check before claiming
  visual verification.

## 2026-09-17: iced front-end vertical alignment, dynamic title replacement, and view purity
- **Text & Decoration Vertical Alignment Fix (`pane_canvas.rs`)**:
  - Root cause: `align_y: Vertical::Top` was passed `run.baseline_y` (`y + cell.baseline`), positioning the top of glyphs at the row baseline and pushing text downward by `ascent` (~14.5px). This pushed text across row boundaries, caused underlines to slice through text, and created highlight gaps.
  - Fix: converted baseline to iced's top-left text origin with `run.baseline_y - crate::measure::ascent(font_px)`, cleanly centering glyphs vertically in `[y, y + cell.h]`. Cell backgrounds are rendered before text runs, and underlines sit cleanly below descenders.
- **Dynamic Titles & Process Tracking (`config.rs`, `pane.rs`, `panes_grid.rs`)**:
  - Added iced configuration overlay (`~/.config/ghostwave/config.toml`): `dynamic_title = "process" | "osc" | "off"` and `pane_title_format = "process" | "pwd" | "both"`, without altering frozen `src/**`.
  - Replaces `pane N` with dynamic title directly (e.g. `lazygit` or `lazygit [1/2]` when stacked), falling back to `pane N` if title is empty or disabled.
  - Window title suffix (`{title} — ghostwave`) is conditioned on `dynamic_title != Off`.
- **View Path Purity & Performance**:
  - `display_title()` / syscalls (`tcgetpgrp`, `/proc/<pid>/comm`, `/proc/<pid>/cwd`) moved off `view(&self)`.
  - Evaluations occur on tick (`AppState::tick`) and cache into `Pane::cached_title`. `view(&self)` (`Tab::label`, `cell_label`, `App::title`) reads `cached_title` with zero syscalls and zero allocations.
- **Scroll Step Normalization**:
  - High-res pixel scroll deltas normalize against actual cell height `(cell.h as f32).max(1.0)` instead of a fixed 15px constant.
- **Code Standards**:
  - Pure rustdoc in source files; technical rationale documented in `PORT-PLAN.md` and `PROGRESS.md`.
  - 23/23 unit and integration tests passing warning-free.

## 2026-09-17: iced front-end M5b (chrome widgets), M5c (scratchpad), and M6 (overlays & content panes)
- **Native Chrome Widgets (`chrome_widgets.rs`)**:
  - Tab bar: native iced chips with active/inactive styling, click-to-focus `Message::FocusTab`, and inline `text_input` tab renaming.
  - Status bar: mode pill, keybind hint line, theme badge, sync chip, and live reactive 2-harmonic wave visualizer canvas.
  - Floating toast notification overlay.
- **Floating Panes & F12 Scratchpad (`scratchpad.rs`)**:
  - Float manager with geometry presets (`TopDrawer`, `Centered`), window boundary clamping, Zellij-style float cycling (`e`/`E`), and origin re-embedding.
- **Command Palette (`palette.rs`)**:
  - Modal overlay with action inventory from `ghostwave::keymap::Action`, installed themes, fuzzy matching, prefix commands (`open`, `ssh`, `ds`), and keyboard navigation.
- **Scrollback Search Overlay (`search_overlay.rs`)**:
  - Compact search overlay widget with regex and case toggles, match counters, and bidirectional terminal search synchronization.
- **Mouse Selection & Clipboard (`selection.rs`)**:
  - Normal, word, and line selection gestures, coordinate translations, text extraction with wide character handling, and native Wayland/X11 clipboard operations via `arboard`.
- **Content Panes Ported to Iced 0.14**:
  - **Visualizer (`visualizer.rs`)**: 3D rolling perspective grid, neon spectral mountain peaks, cyber rain particles, and palette rotation (`GhostNeon`, `MatrixGreen`, `SolarAmber`, `AbyssIce`).
  - **Preview (`preview.rs`)**: Text, directory, hex dump binary, and interactive image canvas viewer with zoom/pan.
  - **AI Chat (`ai.rs`)**: OpenAI-compatible completion assistant with message cards, busy indicator, and non-blocking worker thread.
- **Verification**:
  - `ghost-iced`: 102/102 unit tests passing cleanly with 0 warnings.
  - Release binary built cleanly (`cargo build --release --manifest-path ghost-iced/Cargo.toml`).

### 2026-09-17 (later): integrating the new modules — slice 1, overlays

Those modules were written but reachable from nothing: `app.rs`, `main.rs`, `panes_grid.rs` and
`pane.rs` had no reference to them and every action still raised a "not implemented yet" toast. The
plan and its verified API table are `ghost-iced/PORT-PLAN.md` §19.

- **Command palette** (`Ctrl+Shift+P`): opens over the grid, owns the keyboard while open
  (navigation, query, Enter submits), and executes through the same `input::execute` the key binds
  use — palette entries and binds cannot drift apart.
- **Scrollback search overlay**: opens on `Ctrl+Shift+F`; the query is pushed into the focused pane's
  own `ghostwave::search::SearchState` (`TermPane::search` is public), so the overlay and the
  terminal's match engine share one state, case/regex toggles included.
- **F12 / `Ctrl+~` scratchpad**: toggles real state (spawn/embed a floating terminal).
- Overlays are `Stack` layers above the pane grid; a pane receives no keys while one is open.
- 104/104 tests (102 pre-existing, one updated in place, two new: palette-executes-its-selection and
  search-owns-the-query).

Honest gaps at this point — these are not claims of working features:

- Nothing scrolls the viewport to a search match. The core's search matches **viewport rows only**
  (`src/search.rs` documents scrollback-wide search as a TODO) and `TermPane` exposes no
  scroll-to-match helper, so the overlay syncs state and forces a repaint only.
- The palette's own `text_input` is never focused, so typing is routed through its query API.
- `Action::SearchToggleWrap` flips the core's `wrap` field, which `run_on_rows` does not read: that
  toggle is currently cosmetic.
- Scratchpad and floating panes are state-only — nothing paints a float yet.
- `palette::view` draws a 0.65-alpha full-window backdrop above the grid; per the paint-order traps
  that can suppress the chrome canvas's text pass. Needs a real-window check.
- The preview / AI / visualizer panes still raise their honest toasts; they need the content-pane
  refactor (slice 2).

### 2026-09-17 (later still): integrating the new modules — slice 2, content panes

A grid cell can now hold something that is not a terminal.

- **`PaneEntry = Term(SharedPane) | Content(ContentPane)`** with
  `ContentPane = Preview | Ai | Visualizer`; `Tab.panes` holds entries, and content panes take their
  id from the same `PaneId` counter, so the grid payload and pty routing are unchanged.
- **Every terminal-only path matches on `Term`**: pty subscriptions, key send, resize/`set_grid`,
  dynamic titles, mouse tracking, the sync broadcast, and pty routing by id.
- **Entry points**: the palette's "New AI Chat" and "Ghostwave (Screensaver Visualizer)", and
  `Ctrl+Click` on a path (`Action::OpenFile`) for the preview — each splits the focused cell.
  `ClosePane` closes a content cell with no pty involved.
- **Keys go to a focused content pane** and never to a pty: preview — `j`/`k`, arrows,
  `PgUp`/`PgDn` (20 lines), `+`/`-`/`=`/`_` zoom, `0` reset; AI — text reaches its prompt, `Enter`
  submits; visualizer — `c` cycles the colour mode.
- **AI replies are polled on the tick** (`AppState::tick_content`), because a std `mpsc::Receiver`
  is neither `Hash` nor `Sync` and cannot back an iced subscription; `tick_rate()` returns `Slow`
  while a request is pending and `Fast` while a visualizer is live.
- 111/111 tests (7 new: an AI content cell and no new terminal, a preview entry's kind, a
  visualizer entry, terminal-only paths skipping content, closing a content cell, preview keys,
  typing into a focused AI pane).

Honest gaps at this point:

- The preview's page step is a hardcoded 20 lines; nothing in the plan fixes that number.
- The AI prompt's own `text_input` messages are deliberately dropped (`Message::Noop`) so the key
  path is the single writer and a character cannot be applied twice when the field has focus — the
  cost is that pasting into that field does nothing.
- Floats and the F12 scratchpad are still state-only: nothing paints them.
- None of slice 2 has been seen in a real window yet (content cells, their title bars, the palette's
  backdrop over the grid), so nothing here is claimed as visually verified.

### 2026-09-17 (later still): selection highlight (part of slice 3)

Selecting text with the mouse now paints the highlight: `pane_canvas::paint_pane` fills
`theme.selection` behind each selected cell (`Cell::is_selected()`), which is exactly what the
macroquad build does (`src/term.rs:878` — same call, same colour). Gestures and copy were already in
place from M3 (`Ctrl+Shift+C` → `Action::Copy` → `TermPane::copy_selection()`), so this was the only
missing piece; no core change was needed, because libghostty's render API already exposes the
selection.

`ghost-iced/src/selection.rs` remains in the tree, orphaned and unreferenced: it is a second
selection model with its own `copy_selection(grid)`, and the owner's instruction is that
deduplication happens at the archive cut rather than in an ad-hoc pass now
(`RETIREMENT-PLAN.md` §5 step 7). 111/111 tests still pass.

### 2026-09-17: search fixes from the first real test run

The first hands-on run of the wired overlays turned up two things worth recording:

- **The wrap toggle in the core was a lie.** `SearchState::next_match` / `prev_match` always wrapped
  (`current = (current + 1) % matches.len()`), so the `wrap` field was never read and the UI's wrap
  toggle did nothing. Both now honour the flag; the existing `next_prev_wraps` test still passes
  because the default is `wrap: true`, and `next_prev_stop_at_the_ends_without_wrap` pins the new
  behaviour. This is a core change, so it fixes the macroquad build's toggle too.
- **A pane's filled rectangles do not reach the screen in every pane; its glyphs do.** That, and only
  that, is why the selection highlight was invisible. iced draws a canvas's shapes as triangle primitives
  and its text as glyph primitives; in a pane that had been on screen before a split, only the glyphs
  rendered, so a band drawn with `fill_rectangle` never appeared while the pane's own text did. The
  decisive evidence was the owner's report of an earlier attempt, not the logs: that build inverted the
  glyph colour *and* filled the band, and the owner saw the text disappear while no band appeared — the
  signature of a fill that never renders plus a glyph colour that does. The band is now built from glyphs:
  each selected cell contributes a full block (`BAND_GLYPH`, `\u{2588}`) to a text run at `BAND_ALPHA`,
  emitted through the same run mechanism as the terminal text, with the text drawn over it in its own
  colour. Search-match highlighting still fills, so it remains invisible in that pane until it gets the
  same treatment.
  Lesson: when a framework's rendering surprises you, read the framework's own source and how its own
  widgets do the same job (iced's text editor renders its selection as rectangles; iced's canvas draws
  text above shapes) before guessing at colours, ordering or caching.
- **The cut is done: the iced front-end is the app, macroquad is gone.** The port's sources moved to
  the crate root and became the binary; the macroquad presentation (`app.rs`, `main.rs`, `palette.rs`,
  `ai.rs`, `preview.rs`, `visualizer.rs`) is deleted along with its renderer and kitty-graphics code,
  and `macroquad`/`image` left the dependency list. Three of the port's modules were renamed around
  core modules that stay: `ui_config.rs`, `ui_fonts.rs`, `ui_session.rs`. Two things the cut exposed:
  the port's asset `include_bytes!` paths were one directory too deep once it sat in `src/`, and the
  AI pane's chat request lived in the deleted core `ai.rs` — it was restored from `macroquad-final`.
  One crate now: `lib` = core, `bin` = the iced app. `cargo test`: 49 + 130 + 1 + 6 pass. README
  rewritten so every claim is something this build does, with the unported items (remote/SSH, kitty
  graphics, middle-click paste, preview images, session parking) named in a "Not in this build"
  section pointing at the archive.
- **How the crate move (step 5) is shaped, decided before touching it.** The root package already has a
  **lib** (`src/lib.rs` = the core) plus a **bin** (`src/main.rs` = macroquad), and a bin target can use
  its own package's lib by name — so after the cut the port's sources must move into a **subdirectory**
  (e.g. `src/ui/`), not into `src/` directly: the port has `app.rs`, `input.rs`, `session.rs`, `pane.rs`,
  `theme.rs` and friends, which collide with core files of the same names. The port's `main.rs` becomes
  the root binary, its `mod` declarations gain the subdirectory prefix, `ghost-iced/Cargo.toml`'s
  `[workspace]` marker and `ghost-iced/Cargo.lock` go away, the second `ghostwave = { path = ".." }`
  dependency collapses into the same package, and `use ghostwave::…` keeps working unchanged because it
  still names the lib. Order matters: do 5 (move) then 6 (delete the presentation files) and only then 4
  (de-macroquad the core) — the macroquad-typed surfaces in `term.rs`/`keymap.rs`/`fonts.rs` are still
  used by the macroquad `main.rs`/`app.rs` until those are deleted, so stripping them earlier breaks the
  build on this branch for no gain.
- **Where this session ended (2026-09-17).** The column chrome and the cleanup are committed on
  `feat/widget-chrome` (`30318d5`, `08e5758`) and pushed; `feat/retire-macroquad` holds the retirement
  cut and the archive; `macroquad-archive` / tag `macroquad-final` is untouched by any of it. Neither
  feature branch is merged — **master is protected and the merge is the owner's call.** The release
  binary on `PATH` (`~/.local/bin/ghostwave`) is built from `feat/widget-chrome` and verified by eye.
  Remaining work, in the owner's order: **remote/SSH panes** (the only missing feature; `ghostd/` is in
  the tree, only the front-end half is unported) and then merging the two branches. Everything else
  that was on the parity list is done and verified: selection, search, palette, sessions, floats, the
  F12 scratchpad, the visualizer and AI panes, cursor styles, and the widget chrome. The stale
  branches (`feat/iced-frontend`, `feat/visual-ghostwave`, `perf/idle-gpu-2`, `selection-autoscroll`)
  were checked for unmerged work and deleted locally and on the forge.
- **Cleanup pass: 60 warnings to zero, and the dead canvas chrome deleted.** `chrome.rs` (450 lines) and
  `pane_canvas.rs`'s `TerminalCanvas`/`ChromeData` were unreachable once the chrome became widgets, so
  they are gone, along with `measure.rs`'s width helpers (only `ascent` serves the pane painter now), the
  pane-log byte counter that was assigned but never read, four `term.rs` fields left from the macroquad
  renderer, and the unused imports across the crate; `search.rs`'s `is_word_boundary` is test-only. 12
  files, 64 insertions, 661 deletions, `cargo check` clean, tests 49 + 130 + 1 + 6, no behaviour change.
  Two profiler leftovers (`flamegraph.html`, `perf.data`) removed from the tree root.
- **Window chrome belongs in a column, not in layers — and that is what M5b actually needed.** The swap
  is done in its proper form: `main.rs` builds a `Column` of tab bar → pane grid → status bar, and a
  `Stack` appears only when something pops up (floats, search, palette, sessions, toast). That removes
  the padding hack the grid used to reserve header/footer space, removes the canvas chrome layer from the
  pane band entirely, and makes a bar overlapping a pane structurally impossible. Diff is two files,
  22 insertions / 33 deletions, with `pane_canvas.rs` untouched. Owner-verified in a window.
  Two lessons, both from being wrong first: the previous attempt's overlay came from edits *inside the
  pane path*, not from the layering — the giveaway was that the overlap also appeared with the chrome
  turned off, which should have killed the layering theory on the spot. And a canvas-drawn chrome cannot
  be mixed with widget chrome in one stack: canvas fills paint above widget text (a canvas bar's
  background covered the widget tab labels), while text drawn in a canvas paints above widget rectangles.
- **M5b (widget chrome) was attempted and reverted — and it exposed the rule behind the "fills do not
  render" mystery.** Wiring `chrome_widgets` as its own layer did make the widget chrome live (tab bar,
  status bar, toast, rename prompt all built by widgets), and in doing so proved something the earlier
  diagnosis got wrong: iced batches **every shape in a layer before any text**, across *sibling*
  canvases. The port paints each pane in its own canvas, and cell backgrounds were drawn as block
  *glyphs* (the workaround from the selection bug) — so the newest pane's background glyphs landed on top
  of the older pane's text, hiding its output. That is not "fills do not render": the fills were being
  overpainted by a later canvas's glyphs. Clipping each cell body (`.clip(true)`) fixes the *spatial*
  bleed but cannot reorder text against text; putting cell backgrounds back to shapes fixes the ordering
  but then covered the pane's own later output. Neither combination matched the canvas chrome, and the
  owner called it: reverted (`git checkout -- src`), tree back at `3d44fe3`, and the working release
  binary was never touched because the swap was never committed. `chrome_widgets.rs` (801 lines) stays in
  the tree unwired as the basis for a future attempt. The right shape for that attempt is *not* per-cell
  canvases: one canvas per window painting every pane restores the single global paint order that the
  macroquad build had, which is what makes backgrounds-under-text work at all.
- **Retirement, step by step.** The archive is cut and pushed: branch `macroquad-archive` (one commit on
  top of `8c40aaf` adding `ARCHIVE.md`) and annotated tag `macroquad-final` → `8c40aaf`, verified on the
  remote, at a commit a fresh worktree proved green (66 + 66 + 1 + 6 tests). That kills the only
  irreversible risk in the plan. `STATUS_BAR_H`/`TAB_BAR_H` moved out of `src/app.rs` into
  `ghostwave::theme` (the file the cut deletes), with `app.rs` re-exporting them so the macroquad build
  still compiles and `chrome.rs` importing from the new home; and the port's `forget_meta` is now called
  from `Tab::tombstone`, the single funnel a closed pane passes through. Owner-confirmed in the running
  iced build: selection, search, F12, sessions, floats, **visualizer and AI pane**. Still unexercised by
  hand before the presentation files are deleted: the preview pane, kitty graphics (inline images),
  middle-click paste, hidden/background panes, and the remote/SSH path (deliberately last).
- **Sessions work in the port, and the deadlock that hid them was the pane lock.** Saving read each
  pane's `program`/`args`/`cwd`/`remote`/log name back through the pane's *own* mutex — the same lock the
  pty path takes — so the first pane locked and never returned: every session test hung, and the same
  code would have frozen the app on save or quit. None of it changes after construction, so panes now
  register it in a registry keyed by `PaneId` when they are built (`pane.rs`: `register_meta` /
  `with_pane_meta`) and the session format reads that instead of the pane (`session.rs`). A session
  rename now also persists the renamed session, so renaming one that was never written keeps it.
  130 tests pass with no hangs. Floats took the route the reference implied: `ScratchpadState::to_float`
  on the very `floating` list the float layer paints, with the pane closed out of its grid cell so the
  grid stops painting it, and embedding back by splitting beside the focused cell — the float, hide and
  cycle binds are confirmed working in the iced build (owner, 2026-09-17), as are sessions. Remote panes
  are now the last real parity gap: the reference's `ensure_remote_binary` / `remote_attach_cmd` /
  `connect_remote_pane` / `kill_remote_pane` / `live_remote_specs` subsystem, about 250 lines.
- **What parity actually has left, checked against the reference rather than the toast list.** Three
  arms in `ghost-iced/src/input.rs` still toast, but only two are real gaps:
  `Action::EditScrollback` toasts in the **macroquad build too** (`src/app.rs:502`), so parity there is
  already met and nothing needs porting.
  *Floating panes* (`ToggleFloating`/`ToggleHideFloat`/`CycleFloat`) are a real gap. The pieces are all
  present and no core work is needed: the reference stores floats on the tab (`tab.floating: Vec<…>`,
  with `origin`/`hidden`) and swaps a pane between tree and float using the **shared** core helpers
  `ghostwave::layout::path_to_pane` / `embed_at`; the port already has the float manager and its layer
  (`ghost-iced/src/scratchpad.rs`, painted by `float_layer()` in `main.rs`) plus grid↔`Node` conversions
  (`grid_session.rs`), so the port's job is to move the active pane between the grid and the float
  manager and re-embed at its saved origin on toggle-off.
  *Remote panes* (`ConnectSsh`/`ConnectDurableSsh`/`KillRemoteSession`) are the bigger gap: the reference
  implements a whole subsystem (`ensure_remote_binary`, `remote_attach_cmd`, `connect_remote_pane`,
  `kill_remote_pane`, `live_remote_specs`, plus the dsession naming) — roughly 250 lines, so it should be
  ported as its own piece of work rather than bolted onto the input arms.
- **The iced build is now the app you launch.** `~/.local/bin/ghostwave` was repointed from the macroquad
  release binary (`target/release/ghostwave`) to `ghost-iced/target/release/ghost-iced`, so rofi and
  everything else on `PATH` gets the port. It needs no environment setup: the binary carries its nix
  RUNPATH (`env -u LD_LIBRARY_PATH ldd` reports nothing missing), though those store paths are unrooted,
  so a `nix-collect-garbage` will break the symlink until the build is rooted — the same failure mode as
  the old `libdbus` exit 127. The macroquad binary stays at its old path, and the port starts with one
  pane because session restore is not wired yet.
- **The selection never appeared in an idle pane, and the reason is in the library's own docs.**
  `Cell::is_selected()` reads the row's *cached cell data*, and the C docs are explicit that cell data
  "is only valid as long as the underlying render state is not updated" — while this painter marks every
  row clean right after drawing it (`row.set_dirty(false)`). So a pane that keeps producing output
  re-dirties its rows and showed the highlight, whereas an idle pane kept stale cell data and showed
  nothing: the same code highlighting in one pane and not the other. The painter now takes the range
  from `RowIteration::selection()` (the query the docs recommend for span renderers: `start_x`/`end_x`,
  inclusive, `GHOSTTY_NO_VALUE` when the row misses the selection) and applies it per cell, so row
  dirtiness no longer decides whether a selection is visible.
- **The band colour also had to be derived rather than trusted.** tokyonight's `selection` (`#283457`)
  sits only 0.096 luminance above its `bg` (`#1a1b26`) — invisible in practice, which is what "back to
  nothing" was. The band uses the theme's selection colour when the gap is ≥ 0.15 and otherwise mixes it
  35% toward the foreground; the glyph colour is never touched, so text cannot be swallowed by the band.
- **The port's search could never find anything**, and the cause is in the core's design rather than
  the overlay: matches are computed inside `TermPane::render()` (`src/term.rs:812-819`), which the
  iced front-end never calls because it has its own painter. `search.matches` therefore stayed empty
  and the overlay honestly reported "no matches". The port now runs the same pass itself
  (`Pane::refresh_search`, gated on `search.active() && search.dirty` exactly like the core) and
  paints matches the way the reference does: a matched cell inverts to `theme.search_match` with the
  glyph in `theme.bg`, plus the reference's 2 px row marker at the pane's left edge.
- **The F12 scratchpad was invisible** because its pane is spawned into the tab but given no grid
  cell, so the pane-grid layer had nothing to paint. `main.rs` now has a float layer between the
  chrome and the modal overlays: every `visible_floats()` entry, bottom-to-top by z-index, as a
  sized box at the float's own rect (padding-positioned, since iced has no absolute positioning)
  carrying the same `PaneCanvas` the grid uses — so its painting, its recorded bounds and its mouse
  forwarding are identical to a docked pane — with a title band taken from the pane's cached title
  and a click target that focuses the pane. `the_scratchpad_float_is_visible_to_the_layer` pins the
  layer's inputs (one visible float, its pane in the tab, a non-zero rect, and hidden after F12).
- **Terminal font resolution & monospace cell grid**:
  (1) Resolved font family & weight mismatch in iced: `fontdb` reads Name ID 16 (`TYPOGRAPHIC_FAMILY`),
  indexing `JetBrainsMono-Medium.ttf` as `"JetBrains Mono"` with `Weight::Medium`. The canvas previously
  queried `"JetBrains Mono Medium"` with `Weight::Normal`, causing iced to silently fall back to
  system proportional fonts (Fira Sans/Cantarell).
  (2) Fixed `score_family` in `src/ui_fonts.rs` to reject non-monospace fonts (score 0 instead of 10),
  preventing proportional fonts from polluting the terminal font stack.
  (3) Converted `CellDims` width and height from `u32` to `f32` across `src/term.rs`, `src/pane.rs`,
  and `src/panes_grid.rs` to eliminate rounding drift across large column counts.
  (4) Added user configuration for `font_family` in `~/.config/ghostwave/config.toml` with automatic
  font discovery via `fc-list`, exact cell metric calculation from the selected font, and graceful
  fallback to the bundled JetBrains Mono.

### iced front-end — session durability and pane cwd (branch `feat/widget-chrome`)
- **Sessions are written every two seconds again.** `src/autosave.rs` publishes `Message::Autosave`
  from its own thread and `main.rs` subscribes to it with `Subscription::run_with`, so the write does
  not depend on the animation `Tick` (which is subscribed only while something animates). This is the
  macroquad build's two-second save in `update()` (`macroquad-final:src/app.rs:2878`), which the port
  had dropped — `ui_session.rs` documented its absence. `iced::time::every` is still not available in
  iced 0.14 (no tokio/smol feature), which is why the period lives on a thread.
- **A compositor kill bind now saves.** iced's `exit_on_close_request` defaults to `true`, so a
  `CloseRequested` closed the window without ever reaching the app's save path — which is why Ctrl+Q
  was the only reliable way to keep a session. The builder now sets it to `false` and subscribes to
  `iced::window::close_requests()` → `Message::CloseRequested` → `quit = true`, reusing the existing
  save-and-exit path.
- **New panes open in the focused pane's directory.** `AppState::spawn_cwd()` reads the focused pane's
  live working directory (`/proc/<pid>/cwd`, OSC 7 fallback, kept only if it is an absolute path that
  still exists) and hands it to `Pane::spawn` → `Pty::new` → `Command::current_dir`. It covers split,
  stacked split, new tab, tab reset and the F12 scratchpad; the app's first pane still inherits the
  app's own directory. The pane is read with `try_lock`: the spawn path must never wait on a lock a
  caller may already hold, which is the class of deadlock the pane lock has caused here before.
- **The test suite was not hermetic, and that is what looked like five hung tests.** The suite ran
  against the real `~/.local/share/ghostwave`: it created `iced-binds-*.json` there and deleted the
  user's `default` session on the way out. A session test that opens `default` replays every pane log
  through the VT engine, and one of those logs is at the 32 MiB cap, so the run spent minutes in
  `replay_pane_log` with the test thread at 100% CPU — not blocked, just re-reading the user's own
  output. `ui_session::test_lock()` now points `XDG_DATA_HOME` at a per-process temp dir before any
  test reads a session. Full suite: 130 passed, 0 failed, 31.7 s.

### Remote/SSH panes ported to the iced front-end (branch `wt/remote-ssh`) — suite green

The parity gap this file called the last missing feature is ported, and the whole suite runs green
through the project's own shell: **50 + 154 + 1 + 6 passed, 0 failed**, `cargo test --all-targets
--no-fail-fast` **EXIT=0** — `--lib` 50, `src/main.rs` unittests 154, `tests/flood_throughput.rs` 1,
`tests/term_core.rs` 6 (`--all-targets` runs no doc-tests). Commits on `wt/remote-ssh`: `1e2c589`
(the port), `8a5a000` (prime drain), `c46fb58` (pty drop); 11 files, +1793/−125 against the
merge-base. Not merged and not pushed — the merge is the owner's call.

- **What the port adds.** `src/remote.rs` (new, 433 lines) is the stateless half of the reference's
  `feat/remote-durable` subsystem: `sanitize_host`, the `ghostwave-<host>` / `ssh-<host>` session
  names, `attach_cmd`/`attach_argv` (`sh -c` wrapper, version check, `exec attach || new`
  fallback), `kill_cmd`/`spawn_kill`, `upload_ghostd` (`uname -m` → embedded static musl binary,
  version-checked), `reconnect_delay` (base ×2, capped 15 s), `dedupe_hosts`, `pending`,
  `reconnect_pass`. `src/app.rs` holds the stateful half: `connect_ssh`, `connect_durable_ssh`,
  `ensure_remote_binary`, `connect_remote_pane`, `kill_remote_pane`, `kill_session_remotes`,
  `live_remote_specs`, `live_is_pure_remote`, `set_remote_manual`, `respawn_waiting`,
  `stop_remote_reconnect`, `remote_activity`; `tick()` drains the upload channel and runs the
  reconnect pass. `ConnectSsh`, `ConnectDurableSsh` and `KillRemoteSession` dispatch into these
  instead of toasting. The behaviours this file specified are preserved: bounded auto-reconnect
  (5 attempts, 3 s→15 s backoff, then manual), restored remote panes come back **DISCONNECTED** and
  connect on Enter, the session manager's `ssh` marker, and `k` = kill remote session.
- **`ds <host>` sessions hold only their attach pane.** The port's first suite run failed
  `the_durable_ssh_action_opens_its_own_session_and_binds_the_pane` with `remote_spec_of(0, 0) ==
  None`: `ui_session::start` gives a new session one placeholder shell tab and the attach pane was
  appended beside it, whereas the reference empties the tab list (`park_current` does
  `std::mem::take(&mut self.tabs)`, and `connect_durable_ssh` does `self.tabs = Vec::new()`). That
  is not only test fidelity — a stray local shell makes `live_is_pure_remote` false, so `k`
  (`kill_session_remotes`) would never delete a session it is supposed to delete wholesale. Fixed
  in `src/pane.rs`.
- **`prime_pty` started its idle clock before the child had written anything.** A child whose
  fork/exec+printf took longer than the old 60 ms window was declared idle and the drain returned
  having read nothing — the startup burst prime exists to catch. Measured with a pty probe
  (`g1/pty_latency.py`, 40 samples/run): that child takes p50 3.6 ms idle, but p90 43.4 ms and
  max 268.2 ms with 12 hogs on the box, so the old code lost it 3/40 under load and 0/40 idle —
  which is why it failed in two in-suite runs and passed 15/15 standalone. The idle clock now
  starts on the child's first byte, the blocking bound is 1 s, and a known child exit ends the
  drain at once (`8a5a000`), with the deterministic regression test
  `prime_drain_waits_out_a_slow_first_write` (its child sleeps 200 ms before its first byte).
- **That fix exposed an unbounded `waitpid` in `Drop for Child`.** The next run hung in the bin
  target: `input::tests::locked_mode_swallows_binds_and_only_ctrl_g_unlocks` and
  `input::tests::terminal_only_paths_skip_a_content_pane` sat for minutes with the test thread in
  `do_wait` and a live bash child. `Drop` reaped with a blocking `waitpid`, so dropping a pane whose
  child ignores SIGHUP/TERM parked the dropping thread forever. `c46fb58` bounds the wait and
  SIGKILLs what will not go. This is a latent deadlock in the iced front-end rather than test
  fallout: any pane closed while its shell refuses to die could have hung the UI.
- **Not exercised by hand, and marked as such.** No real ssh target was used, so
  `ensure_remote_binary`'s upload, the `ghostd attach` path, the reconnect pass and `k`-kill rest on
  their unit tests and the reference's semantics: `NEEDS-HOST:` a real ssh host (a local `ssh orac`
  would do) before any of it is called proven. Nothing was run in a window either, so the `ssh`
  badge, the disconnected/reconnecting label band and the `k` bind are `NEEDS-DISPLAY:`. `ghostd/`
  and the embedded musl binaries are untouched by this work.
- **Build environment note for the next session.** The suite ran through the build gate in the
  *user* manager's namespace (`~/.hermes/scripts/usermgr_build.sh`, `CARGO_TARGET_DIR=
  /mnt/myjfs/build/ghostwave`) because the gateway's namespace has `/mnt/myjfs` read-only. The
  final link of the two ~500 MB test binaries took ~1 h 45 m and it was not CPU: `ld` sat in `D` on
  `folio_wait_bit_common` and the loopback `juicefs` daemon was the only busy process on the box.
  Root was at 96 % the whole time and the mount runs `--free-space-ratio 0.15`, so the write-back
  cache was throttled throughout — expect the same wall-clock for any future full-suite run here.

### OSC 7501 Program Status Protocol, consumer side (branch `wt/osc7501`) — parse, per-pane records, lifetimes, iced surfacing

The owner asked for OSC 7501. This is ghostwave as the **consumer**: it parses the
protocol out of each pane's pty stream, keeps a record table per pane, honours the
lifetimes a multiplexer has to decide for itself, answers feature detection, and
shows the state in the chrome and in an inbox.

**Spec revision pinned: 0.4 (2026-10-10)**, the live revision at
`https://www.superlogical.com/rex/docs/build/program-status`. The card quoted 0.2
(2026-10-06); the parsing grammar, the record/id rules, the states, the keys and
every limit in the table are **byte-for-byte the same text** in 0.2 and 0.4, so
implementing 0.4 implements 0.2's core. Three deltas:

- **0.3** — absent `progress` means *indeterminate* (the key table already said so);
  a program MAY send reports without querying first.
- **0.4** — the feature-detection reply MAY carry `states=` and `kinds=` lists. We
  send them: `ESC ] 7501 ; ?:states=idle,working,done,blocked,error:kinds=permission,question,auth ST`.
  0.2 only asked that the query be answered, and a program reads an answer rather
  than a fixed byte string, so this is legal under both and is what a 0.4-era
  emitter checks to decide whether a newer state exists.
- **0.4 removed the terminfo capability** (`Pst`) that 0.2 defined — and the card's
  item 4 asked us to advertise it "only if we actually implement the query". We do
  implement the query, and 0.4 says there is no terminfo entry to ship, so **no
  terminfo entry is shipped**. `Pst` also goes stale over SSH and in multiplexers,
  which is exactly why the spec now requires a program to query instead.

#### What is implemented

- `src/program_status.rs` (no new dependencies, std only): `Scanner` — a
  fragment-tolerant byte scan for `ESC ] 7501 ; … ST|BEL`, bounded by the spec's
  4096-byte whole-sequence limit, which also reports `OSC 133 ; A` (prompt mark)
  and `ESC c` (RIS); `Table` — the records, whole-record replacement, `clear`
  with and without an id, app inheritance up the id tree, LRU eviction;
  `Tracker` — the two together with the lifetimes and the rate-limited answer.
- `src/term.rs`: one `Tracker` per pane, fed from **both** paths pty bytes take —
  `TermPane::poll_pty` (priming/restore) and `Pane::ingest` (the iced event path) —
  through a single `TermPane::ingest_bytes`. The bytes the VT engine receives are
  unchanged: the scan reads the same slice, it never filters or rewrites it.
- Chrome: the pane's label band carries the state (glyph, state, kind, progress) in
  the state's colour, and a tab's chip shows the glyph of the most urgent record any
  of its panes holds — visible without focusing the pane.
- `src/status_overlay.rs`: the inbox (`Ctrl+Shift+S`, also in the command palette) —
  every live record across every pane, blocked first, each row naming the terminal it
  came from, `Enter` jumping to that pane.

#### Lifetimes, and the multiplexer decisions the spec leaves open

The spec has **no multiplexer guidance** (open issue 1 in the third-party review), so
these are decisions with reasoning rather than readings of the spec:

- **Consume, never forward, never re-emit.** A report is kept in the pane's own table.
  Forwarding is meaningless here (ghostwave is a GUI, not a pty link to an outer
  terminal) and re-emitting on the pane's behalf is rejected outright: it would
  attribute a program's state to ghostwave, double-report when a pane runs another
  terminal, and put bytes on ghostwave's own stdout that no one asked for. ghostwave is
  the terminal of record for its panes; that is the whole job.
- **Nesting (a pane running a terminal, a nested ghostwave, or ssh).** Whichever
  emulator is *attached to the pty the program writes to* is the terminal of record
  and consumes the sequence. So: `ssh host prog` → the reports arrive over the outer
  pane's pty (that channel is why the protocol exists) and ghostwave records them for
  that pane. A pane running *another emulator* → that emulator consumes them and its
  own chrome shows them; the outer pane records nothing, which is correct, because the
  program's terminal is the inner one. Per spec, "a program writes to a terminal" —
  one terminal, one consumer.
- **OSC 9;4 is not mapped.** The spec says a terminal MAY map OSC 9;4 to the root
  record and SHOULD stop once a 7501 report arrives. We never mapped it, so there is
  nothing to stop: a mapped 9;4 can therefore never wipe the `kind`/`msg` of a real
  report.
- **Child exit / prompt begins** — `working` and `blocked` records are dropped;
  `idle`, `done` and `error` stay (the spec MUSTs the first two, MAYs `idle`; we keep
  `idle`). **RIS** clears every record (both from the byte scan and from ghostwave's
  own `clear_terminal`, which calls `Terminal::reset`); a soft reset (DECSTR) and a
  primary/alternate screen switch touch nothing, because nothing in ghostwave's
  paths does. **Closing a pane** removes its records: they live in the pane's table,
  and the inbox is built from the live panes, so a closed pane cannot be listed.
- **What "process exits" means over SSH** (open issue 2) is answered as: the pane's
  pty child. For a remote pane that is the ssh client/ghostd, so a remote program's
  `working` record goes when the session ends — the scope the pane can actually see.

#### Prompt marks: what the pin gives us (item 5 of the card)

libghostty-vt 0.2.1 exposes **no event** for OSC 133: the callback set
(`on_pty_write`, `on_bell`, `on_enquiry`, `on_xtversion`, `on_title_changed`,
`on_pwd_changed`, `on_size`, `on_color_scheme`, `on_device_attributes`,
`on_clipboard_write`) has no reset or prompt hook, and `osc::CommandType::SemanticPrompt`
carries no data. The binding does expose row-level prompt state
(`render::RowIterator` → `Row::semantic_prompt()` / `Cell::semantic_content()`), but
that is a polled view of the visible screen — "a new prompt began" would mean diffing
rows every frame and would miss a prompt that scrolled away, so it is not a lifetime
signal.

Because our scan runs on the same bytes, it can see the mark itself: `OSC 133 ; A` is
detected in the pty stream and drives the same lifetime as the child exiting. **The
limitation, recorded as the card asks:** this fires only when the pane's shell emits
OSC 133 prompt marks (shell integration on); with integration off, the prompt-begins
lifetime is not available and the lifetime falls back to child exit plus an explicit
`state=clear` — which is what the card told us to do if the pin gives us nothing, and
it is why both paths are implemented rather than one.

#### LRU eviction and `blocked` (open issue 3)

256 records per terminal (the spec's cap; terminals must support at least 64 — the
table will not go below 64 even if asked). Above the cap the **least-recently-updated
`blocked` record is pinned**: eviction takes the least-recently-updated *unblocked*
record, and falls back to plain LRU only when every record is blocked, because a table
that refuses to evict cannot stay bounded and the spec makes eviction mandatory. Two
counters make the trade-off visible rather than silent: `Table::evicted` (surfaced in
the inbox title as "N evicted at capacity") and `Table::blocked_count()`.

#### Untrusted text

`msg` and `title` are decoded, refused if they hold a control character (whole report
dropped, per spec), stored as sent, and then **disarmed at the display edge**
(`program_status::sanitize` replaces bidi overrides/isolates, zero-width and other
invisible formatting, and the Unicode line/paragraph separators with U+FFFD) and capped
(240/80 chars). They are drawn only through iced's plain-text widget — no markup path
exists — and every row names the pane it came from. **Nothing in a record can cause an
external effect**: no desktop notification, no sound, no write, no pty output beyond the
constant feature-detection reply, so the only thing rate-limited is that reply (one per
250 ms per pane; a query inside the window is dropped, not queued). A report's content
is provably never echoed: the reply is a `const` byte string, pinned by a test.

#### Verification

`src/program_status.rs` is std-only by design, so its 41 tests run without cargo:
`rustc --edition 2024 --test src/program_status.rs` → **41 passed, 0 failed** (conformance
to the spec's example; unknown key; duplicate last-wins; malformed pair skipped; missing
and unknown `state`; invalid base64; control character in decoded text; `msg`/`title`
over their limits (at the limit and one past); over-long key; `id` past 8 levels/128
bytes/32 per segment; `progress` non-numeric/out-of-range/at the edges; `kind` only with
`blocked`; `clear` with and without an id; whole-record replacement; app inheritance and
a missing parent; LRU at capacity; blocked-pinned eviction; the dominant-state order;
child-exit and RIS lifetimes; a sequence split at **every** byte boundary and
byte-by-byte; an over-long sequence dropped without losing sync; an abandoned body; the
answer being the constant and never report content; rate limiting).

`tests/program_status_pane.rs` drives the same protocol through a **real pane**: the
spec's own shell function in a real `/bin/sh` on a real pty, asserting the working→done
state change, that a 7501 report renders nothing on the grid, that `done` survives the
child's exit while `working` does not, that a report split over pty reads is still found,
and that a `OSC 7501 ; ? ST` query is answered over the pty byte-for-byte.
**That binary and the crate-wide `cargo test` are written but NOT yet run** — see the
blocked note below; nothing in this section claims otherwise.

#### Blocked verification (environment)

The crate build cannot run on orac at the moment of writing: `/mnt/myjfs` is mounted
**read-only** (`JuiceFS:my-jfs-volume … ro`), so (a) the build gate cannot create/take
its lock file (`build_gate.sh: : Read-only file system`, exit 1) and (b) this card's
`CARGO_TARGET_DIR=/mnt/myjfs/build/ghostwave-osc7501` cannot be created. `/` itself has
~2.3 GB free and the existing debug target on that mount is 4.8 GB, so there is no
writable place with room for the dependency tree, and remounting needs root. The
crate-wide `cargo test` (including `tests/program_status_pane.rs` and the iced
front-end's own tests) is therefore **unverified**, as is the UI: the chrome chip, the
tab glyph and the inbox compile untested, and whether the front-end can be driven
headlessly on orac for a screenshot was not reached.
