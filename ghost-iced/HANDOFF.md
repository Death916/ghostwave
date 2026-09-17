# Handoff: ghostwave iced front-end

Written for whoever picks this up next (human or agent). Read this first, then
`PORT-PLAN.md` §13 (cold-start detail), §16 (direction change) and §17 (the plan of record).
`PROGRESS.md` at the repo root is the running history.

## 1. Where things stand

`ghostwave` is a terminal emulator built on `libghostty-vt`, with a macroquad front-end in
`src/**` (the original, complete, works) and a second front-end being ported onto **iced 0.14**
in `ghost-iced/`. The iced front-end is a work in progress on a feature branch; it renders and runs,
and its pane layer *and* pane view are on iced's `pane_grid` as of M5a.

- repo: `~/Documents/code/vibed/ghost`
- branch: `feat/iced-frontend`, tip `e8e7181` ("fix text/cursor baseline alignment & add dynamic
  process titles"), on top of `e29067d` (the pane_grid view) and `e492c2c` (pane background / title plan)
- public mirror: `git.death916.xyz/death916/ghostwave` (`master` is protected; work goes on
  `feat/*` branches and merges only when the owner says so)
- state: **M5a parts 1 and 2 landed** — the pane layer *and* its view are on iced's `pane_grid`;
  test suite green (22 passing as of that commit); `ghost-iced` compiles warning-free
- the macroquad front-end is **being retired**: archived to a `macroquad-archive` branch plus a
  `macroquad-final` tag and removed from the ongoing branches once the iced build covers the feature
  gate in `../RETIREMENT-PLAN.md` §2. Its *features* are the gate until then; its looks stopped
  being the reference (`PORT-PLAN.md` §16/§17)
- the `src/**` freeze is **lifted** (owner's call, 2026-09-17): the shared core may take *additive*
  changes for the port's needs. Do not rewrite shared code the macroquad build still depends on
  until that build is actually removed.

## 2. What "done" means here (read before proposing work)

Parity with the macroquad build's **pixels** is *not* the goal — that was explicitly dropped. The
owner prefers iced's text rendering and knows iced better, so the rule is: **use iced's own
built-ins wherever possible; hand-roll only where a widget makes the look worse.** Aesthetics
matter (the current look is liked), and the acceptance target for the pane work is the feature
table in `PORT-PLAN.md` §17 ("Parity checklist for M5a").

Past the pane work, the gate for **archiving the macroquad build** is the feature list in
`../RETIREMENT-PLAN.md` §2: everything that build can do has to exist in the iced front-end first.

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

Env knobs: `GHOST_ICED_PANES=N` (spawn N panes at startup instead of one) and
`GHOST_ICED_FIXTURE="<command>"` (a shell command to run in each pane). The old `GHOST_ICED_PANEGRID`
A/B knob is **gone** — the grid is the only pane path.

Config: `~/.config/ghostwave/config.toml`. The port reads its title options through its own overlay
(`ghost-iced/src/config.rs`): `dynamic_title` = `"process"` (default) / `"osc"` / `"off"`, and
`pane_title_format` = `"process"` / `"pwd"` / `"both"` (`PORT-PLAN.md` §18). Those keys belong in the
shared `Config` now that `src/**` is unfrozen.

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
- the pane view is one `PaneGrid` borrowing the tab's own state: a canvas per cell showing the
  cell's active member, a stack-aware title-bar label, `on_click` focus, `on_resize`/`on_drag`
  wired, ptys sized from the bounds iced actually lays out, and pane origins rounded so glyphs stay
  sharp on fractional split ratios
- dynamic pane/tab/window titles from the running foreground process (`tcgetpgrp` + `/proc`),
  cached on the tick so the view path stays syscall-free — configurable, see §4
- text and cursor baseline alignment fixed (`run.baseline_y - measure::ascent(font_px)`)
- suite green (22 tests as of `e8e7181`): M3 acceptance tests driving `input::key_press` against
  real panes, session-conversion round-trips, grid-op tests, font tests — all in-crate, no display

## 6. The next task: M5a polish, then M5b

M5a parts 1 and 2 have landed — the pane layer in `app.rs` and the view in `panes_grid.rs` — so the
steps that used to live here are done. What is left, in the order `PORT-PLAN.md` §18 lists it:

1. **M5a polish** — the items §18 names: cell backgrounds painted before the text runs of the same
   pass, the title cache maintained on the tick, and a pass over the pane chrome (frames toggle,
   active highlight) now that the hand-painted canvas layer is gone.
2. **Reconcile the config** — move `dynamic_title` / `pane_title_format` out of the port's overlay
   (`ghost-iced/src/config.rs`) into the shared `Config` as additive `#[serde(default)]` fields; the
   `src/**` freeze is lifted.
3. **Walk the §17 parity table in a real window** and fix what fails — above all the interactions a
   test cannot press (click focus, drag-resize, drag-move) and the pane features never exercised
   (frames toggle, sync, break-to-tab).
4. **M5b** — chrome and overlays as iced widgets (`stack`, `container`, `text_input`, `pick_list`,
   `mouse_area`), which retires `measure.rs` and the hand-painted chrome geometry.
5. Then **M6** (selection/clipboard, search overlay, command palette, the preview / AI / visualizer
   panes, the F12 scratchpad) and **M7** (the numbers). Sessions and SSH are held by the owner.

`ghost-iced/src/grid_session.rs` (Node ↔ `State<GridPane>`, 5 round-trip tests) exists for the
session milestone and has no live caller yet — its `#[allow(dead_code)] mod` in `main.rs` is
deliberate. The grid is the only pane path; the A/B period ended with part 2.

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
- **Canvas text draws from the top of the run, not from a baseline.** Every row sat a few pixels low
  until the offset was applied explicitly: `run.baseline_y - measure::ascent(font_px)`.
- **Cell backgrounds must be painted before the text runs of the same pass.** All text renders above
  all shapes inside a frame, so a background drawn afterwards hides the glyphs it belongs to.
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
- Later milestones, now a single roadmap in `PORT-PLAN.md` §13/§18: M5b chrome and overlays as
  iced widgets, M6 selection/clipboard, search, palette, the preview / AI / visualizer panes and the
  F12 scratchpad, M7 the numbers. The macroquad front-end is archived once the
  `../RETIREMENT-PLAN.md` §2 gate is met — that decision is made, not voted on by a perf number.

## 10. File map (`ghost-iced/src/`)

    main.rs         boot, message loop, key listener, chrome data, grid + chrome assembly
    app.rs          AppState/Tab, the tab-owned pane_grid::State<GridPane>, every state op
    input.rs        the M3 dispatch mirror (key_press/key_release/repeat) + acceptance tests
    pane.rs         Pane/SharedPane, PaneId, ingest/exited/key_press/resize, mouse-tracking flag,
                    foreground-process + cwd inspection for dynamic titles
    pane_canvas.rs  per-pane painter (paint_pane), run-merged text, label helpers, geometry constants
    panes_grid.rs   the PaneGrid view: one canvas per cell, title-bar label, on_click/on_resize/on_drag
    chrome.rs       tab bar / status bar / wave / toast painting
    config.rs       the port's config overlay (dynamic_title / pane_title_format)
    keys.rs         iced Key/Physical/Mods → libghostty KeyPress
    pty_bridge.rs   pty reader → iced stream, tagged per PaneId
    tick.rs         animation tick thread (no iced::time::every in 0.14)
    fonts.rs        fc-list fallback scoring + tests
    measure.rs      canvas text measurement (iced canvases cannot measure text)
    grid_session.rs Node ↔ State<GridPane> conversions for session save/load (tested, no caller yet)
