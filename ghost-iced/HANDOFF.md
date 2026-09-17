# Handoff: ghostwave iced front-end

Written for whoever picks this up next (human or agent). Read this first, then
`PORT-PLAN.md` §13 (cold-start detail), §16 (direction change) and §17 (the plan of record).
`PROGRESS.md` at the repo root is the running history.

## 1. Where things stand

`ghostwave` is a terminal emulator built on `libghostty-vt`, with a macroquad front-end in
`src/**` (the original, complete, works) and a second front-end being ported onto **iced 0.14**
in `ghost-iced/`. The iced front-end is a work in progress on a feature branch. It renders and
runs; the pane layer is mid-refactor.

- repo: `~/Documents/code/vibed/ghost`
- branch: `feat/iced-frontend`, tip `6b0807c` ("iced: pane_grid owns the pane layer (M5a part 1)")
- public mirror: `git.death916.xyz/death916/ghostwave` (`master` is protected; work goes on
  `feat/*` branches and merges only when the owner says so)
- state: **tests 19/19 pass, `ghost-iced` compiles with zero warnings, working tree clean**
- nothing of ours is running: no iced test windows, no macroquad instance, no subagents

## 2. What "done" means here (read before proposing work)

Parity with the macroquad build's **pixels** is *not* the goal — that was explicitly dropped. The
owner prefers iced's text rendering and knows iced better, so the rule is: **use iced's own
built-ins wherever possible; hand-roll only where a widget makes the look worse.** Aesthetics
matter (the current look is liked), and the acceptance target for the pane work is the feature
table in `PORT-PLAN.md` §17 ("Parity checklist for M5a").

## 3. Never touch

- `src/**` and the top-level `Cargo.toml` — the macroquad build must keep working side by side.
  `git diff --stat -- src Cargo.toml` must stay empty in any iced commit.
- `~/.local/share/ghostwave` — the owner's real data. Test runs use `XDG_DATA_HOME=/tmp/...`;
  a shared data dir lets one instance's log pruning unlink another's live pane logs.
- `~/Documents/nix-config/` (unrelated to this project, but sacred).

## 4. Toolchain and exact commands

Everything runs through the owner's dev shell. Never `nix run`, never `nix profile install`,
never a bare tool invocation, never download packages.

    # check / test / build (target dir is shared and warm — do not cargo clean)
    cd ~/Documents/code/vibed/ghost && \
      CARGO_TARGET_DIR=/tmp/icedspike-target XDG_DATA_HOME=/tmp/ghost-iced-test \
      nix-shell ~/.hermes/shell.nix --run \
      "cd ~/Documents/code/vibed/ghost/ghost-iced && cargo test --message-format=short"

    # run the iced front-end (debug binary; LD_LIBRARY_PATH comes from the dev shell)
    cd /tmp && GHOST_ICED_PANES=2 XDG_DATA_HOME=/tmp/ghost-iced-a \
      LD_LIBRARY_PATH="$(cat /tmp/iced-spike-ldp)" \
      /tmp/icedspike-target/debug/ghost-iced

    # run the macroquad front-end (the reference to compare behaviour against)
    cd ~/Documents/code/vibed/ghost && nix-shell ~/.hermes/shell.nix --run "cargo run"

Env knobs the iced binary understands today: `GHOST_ICED_PANES=N` (spawn N panes instead of one),
`GHOST_ICED_FIXTURE="<command>"` (shell command to run in each pane), `GHOST_ICED_PANEGRID=1`
(use the pane-grid path instead of the hand-painted canvas path — **this knob is scheduled for
deletion by the next task**, see §6).

Versions: rustc 1.95.0; iced 0.14.0 (`iced_widget` 0.14.2, `iced_wgpu` 0.14.0, wgpu 27.0.x,
cosmic-text 0.15.0); libghostty-vt 0.2.1. Crate sources for reading (not the docs site, which
lags): `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`.

Environment: Hyprland/Wayland, monitor scale 1.25, iced windows have an empty window class
(`hyprctl clients -j | jq '[.[] | select(.class=="")]'`). Screenshots: `grim -g "<x>,<y> <w>x<h>"`.

## 5. What works now (verified, not claimed)

- boot, window, resize handling; the pty seam (`TermPane` + iced-side render scratch), key
  encoder, font stack (embedded JetBrains Mono + `fc-list` fallbacks, `Shaping::Advanced`)
- one tab, N panes, per-pane ptys and logs, pane labels, frames/divider, active-pane highlight
- chrome: tab bar (chips + rename prompt), status bar (mode pill, keybinding hints, theme name),
  two-harmonic wave, toast — painted per `CHROME-REFERENCE.md`
- the whole M3 input layer: pane/tab/resize/move/scroll/lock modes, globals, `Ctrl+q`, and honest
  "not implemented yet" toasts for unported features (palette, search, sessions)
- tabs: new/close/goto/rename/move/last-tab; pane ops: split down/right, stack, close, zoom,
  frames toggle, sync, break-to-tab — **these route through iced's `pane_grid::State` now**
- 19 tests: 13 M3 acceptance tests driving `input::key_press`, 5 session-conversion round-trips,
  1 grid-ops test (all in-crate, no display needed)

## 6. The next task: M5a part 2, the pane view

Part 1 (landed) made `pane_grid::State<GridPane>` the layout authority in `app.rs`; the *view* is
still the first cut, so the pane layer is not yet at parity. Do this, in order:

1. `panes_grid.rs`: build the `PaneGrid` from the tab's own `State` (borrow it — no cache, no
   re-derivation). Per cell: one canvas painting the cell's **active member** via
   `pane_canvas::paint_pane` (already `pub(crate)`), plus a `TitleBar::new` label. Wire
   `.on_click` → focus the clicked cell's active `PaneId` (map through `Tab::index_of`),
   `.on_resize`/`.on_drag` → the existing `Message::GridResize`/`GridDrag` (already routed to
   `State::resize`/`State::drop`). Style: 1px `theme.frame` / 2px `theme.frame_active` outline,
   16px label band in the frame colour with `theme.tab_bg` text, 4px gutter showing the window
   background. A stack's label says which member is shown and that there are more.
2. Delete the legacy layer: the multi-pane mode of `TerminalCanvas` (per-pane rect list, the
   `Frame::with_clip` loop, hit-testing, the press-to-focus flag) and the `GHOST_ICED_PANEGRID`
   knob. Keep `paint_pane`, the label helpers, the geometry constants, and the chrome painter
   (tab bar/status bar/wave/toast) as a separate canvas layered **above** the grid as the second
   `Stack` child with `paint_background: false`.
3. Pty sizing follows iced's layout: the pane's canvas records the bounds it was drawn with, the
   update path turns that into cols/rows with the existing cell metrics and calls
   `TermPane::set_grid` (no-op when unchanged). Remove the remaining `AppState::pane_rects()`
   callers so nothing derives rects in parallel.
4. Paint each pane from a **rounded origin** (`bounds.x.round()`): ratio-based splits produce
   fractional bounds (observed `556.80005`) and un-rounded origins blur the glyphs. This is the
   visible sharpness difference between the two windows.
5. Tests: keep all 19 passing; add pure-function tests for bounds→cols/rows (including fractional
   and too-small), the stack label string, and the click→focused-pane mapping. Nothing that needs
   a display.
6. Verify: `cargo test` per §4, then a real-window run and the §17 parity table row by row.

`ghost-iced/src/grid_session.rs` (Node ↔ `State<GridPane>`, 5 round-trip tests) exists for the
session milestone and has no live caller yet — its `#[allow(dead_code)] mod` in `main.rs` is
deliberate. After this task the grid is the only pane path (the A/B period ends).

## 7. Pitfalls already paid for (do not rediscover these)

- **`pane_grid` must own the layout.** A `pane_grid::State` derived from an external tree does not
  line up with what iced draws: measured, `on_click` reported the first pane for every click (a
  press in the right cell published `Pane(0)`), so focus snapped to the wrong pane. Either the
  grid owns the layout (it does now) or do not use `pane_grid`. Do not re-add a derived state.
- **Paint order in iced.** A `Frame` keeps shapes and text in separate passes and renders all text
  above all shapes; a `Frame`'s *own* primitives are flushed *after* everything drawn inside
  `Frame::with_clip`. Consequences: chrome must be the top `Stack` child, and a window-background
  fill must live in its own region or it covers the panes. "Who paints on top" is not call order.
- **A layered chrome canvas must not paint the window background** (`paint_background: false`), or
  it hides everything beneath it.
- **Transient keys break lookups that resolve by them.** `pop_focused_stack` removed a pane from
  the cell's payload before `focused_id()` looked the cell up by "which cell contains the focused
  pane"; the lookup then returned `None` and the focus stayed on the closed pane. Capture the
  handle, mutate, then read the post-mutation value.
- **iced 0.14 API deltas**: no `iced::time::every` (tick thread + conditional
  `Subscription::run_with`, rate must hash), no `keyboard::listen` (`event::listen_with`),
  `Task` not `Command`, canvas needs `type State: Default`, `Color::new` is private, canvas has no
  text measurement (that is what `measure.rs` is for).
- **Canvas text needs three explicit choices**: register the font via
  `iced::application(..).font(bytes)` and name it, use `Shaping::Advanced` (Basic = no fallback =
  tofu), and draw runs of same-colour cells rather than one text item per cell.
- **Do not trust a vision reading of a screenshot over the owner's eyes**, and check for
  colour-shifted captures before concluding anything from pixels.

## 8. Conventions the owner enforces

- Sources carry **rustdoc only** (`//!`, `///`) — summaries, units, invariants. Rationale,
  history, agent notes and cross-references to the macroquad build go in the `.md` files. The
  owner runs `cargo doc` / an md generator over the comments.
- `PROGRESS.md` gets a status line at each milestone; `PORT-PLAN.md` carries the plan of record
  and the decisions; `CHROME-REFERENCE.md` is the line-cited chrome spec.
- Commits: the owner decides when. WIP commits on `feat/*` are fine when asked for; never commit
  to `master`; never claim a feature works that has not been exercised.
- All tooling through `nix-shell ~/.hermes/shell.nix` (see §4).
- "Investigate/check/look at" means **read only**: no edits, no commits, no builds, then report.

## 9. Open risks and unknowns (be honest about these, do not paper over them)

- Drag-resize and drag-move are new interactions (the macroquad build has no equivalent) and have
  never been vetted in a real window.
- Idle CPU has not been measured; the wave animation drives a 16 ms tick while energized and
  125 ms when idle (and is off entirely with `visual_wave = false`).
- Stacks exist as a payload shape (`GridPane { panes, active }`); rendering their non-active
  members, and floating panes, are unimplemented (floats are M5c).
- iced 0.14's API moved twice during this port (keyboard, time, canvas); expect churn.
- Later milestones: M5b chrome/overlays as widgets (`stack`, `container`, `text_input`,
  `pick_list`, `mouse_area`), M6 selection, M7 the parity numbers that decide whether the iced
  front-end replaces the macroquad one.

## 10. File map (`ghost-iced/src/`)

    main.rs         boot, message loop, key listener, chrome data, grid + chrome assembly
    app.rs          AppState/Tab, the tab-owned pane_grid::State<GridPane>, every state op
    input.rs        the M3 dispatch mirror (key_press/key_release/repeat) + acceptance tests
    pane.rs         Pane/SharedPane, PaneId, ingest/exited/key_press/resize, mouse-tracking flag
    pane_canvas.rs  per-pane painter (paint_pane), label helpers, geometry constants, chrome painter
    panes_grid.rs   the PaneGrid view (first cut; the rewrite target of §6)
    chrome.rs       tab bar / status bar / wave / toast painting
    keys.rs         iced Key/Physical/Mods → libghostty KeyPress
    pty_bridge.rs   pty reader → iced stream, tagged per PaneId
    tick.rs         animation tick thread (no iced::time::every in 0.14)
    fonts.rs        fc-list fallback scoring + tests
    measure.rs      canvas text measurement (iced canvases cannot measure text)
    grid_session.rs Node ↔ State<GridPane> conversions for session save/load (tested, no caller yet)
