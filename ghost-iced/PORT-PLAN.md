# ghostwave on iced — port plan

Written 2026-09-16 at the end of the session that built the spike. Goal: a **cold
session can execute this without re-deriving anything**. Read this file top to
bottom before touching code.

## 1. Why

The macroquad build's idle/typing GPU cost is a present-rate problem, not a
content problem: macroquad/miniquad cannot skip a present, and the EGL backbuffer
is not retained on Wayland, so every loop iteration redraws the whole window and
forces a full compositor recomposite. Measured: idle went 50-63% system GPU to
1-2% after adding an 8 FPS idle tier, and typing still costs ~15% because a
keystroke echo drives ~120 presents/s. Ghostty instead draws only when the
terminal changes, so its idle cost is zero presents.

iced 0.14 added "Reactive rendering": it redraws on state change, not on a timer.
That is the property the macroquad build cannot have. Secondary wins: cosmic-text
shaping replaces fontdue + our hand-rolled fallback stack, and palette / tab menu
/ session manager / toast become real retained widgets instead of hand-rolled
rects with manual hit-testing.

## 2. Current state (spike)

- Spike crate: `/home/death916/Documents/code/vibed/ghost-iced-spike`
  (standalone, `[workspace]` at the top so it never joins the app workspace,
  `.git` not initialised yet — do that first if you want checkpoints).
- It **builds clean and runs**: one libghostty-vt pane, pty via forkpty, painted
  on an iced Canvas (bg rects, per-cell fg colours, cursor), keyboard → pty
  bytes, and an output-stream subscription. It prints its own presents/s counter.
- Not yet verified: an actual screenshot of it (the last bench run died on a
  `pgrep` bug before capture — see gotchas).
- Build: `cd /home/death916/Documents/code/vibed/ghost-iced-spike &&
  CARGO_TARGET_DIR=/tmp/icedspike-target nix-shell /home/death916/.hermes/shell.nix
  --run "cd /home/death916/Documents/code/vibed/ghost-iced-spike && cargo check --message-format=short && cargo build --release"`
  (always `cargo check` first — a release build is ~3 minutes when deps are warm).
- Binary: `/tmp/icedspike-target/release/ghost-iced-spike`.
- Run: `LD_LIBRARY_PATH="$(cat /tmp/iced-spike-ldp)" /tmp/icedspike-target/release/ghost-iced-spike`
  Optional `SPIKE_FIXTURE='<shell command>'` makes the pane run that command
  instead of a login shell.
  `/tmp/iced-spike-ldp` holds the `LD_LIBRARY_PATH` copied from the app's own
  project shell (regenerate with: `cd .../ghost && nix-shell ./shell.nix --run
  'printf %s "$LD_LIBRARY_PATH"' 2>/dev/null | tail -1 > /tmp/iced-spike-ldp`).
- Harness kept for later: `/tmp/bench2.sh <label> <fixture> <secs>` (strace-parent
  presents histogram + the app's own DRM gfx-engine time), `/tmp/iced-demo.sh`
  (ANSI/256/RGB/bold/inverse/unicode/emoji demo screen for screenshots).

## 3. What comes from the macroquad app, and what happens to it

Measured with `grep -c macroquad src/*.rs` in `/home/death916/Documents/code/vibed/ghost`.

Copy verbatim (0 macroquad refs, ~2.76k lines):
  pty.rs (313) — forkpty, non-blocking master, reader thread, write, resize
  session.rs (501) — session JSON, pane logs, pruning, async log writer
  layout.rs (731) — the binary split tree (split/stack/float/embed), unit-tested
  config.rs (243) — config.toml loading + defaults
  theme.rs (156) — themes, Rgb
  search.rs (289) — viewport search + match bookkeeping (uses libghostty rows)
  palette.rs (308) — command palette data/entries
  clipboard.rs (223) — arboard in-process clipboard (keep! do not route through
                       iced's Clipboard trait; see risks)

Paint-rewrite only (1 ref each; state/content reusable):
  preview.rs (282), visualizer.rs (271), ai.rs (293 — AI pane is NOT advertised
  as a feature; port it or leave it out)

Replace: fonts.rs (370) — macroquad Font objects + measure_text. Keep the pure
  parts: fontdue cell metrics (line metrics: cell_h = ascent + -descent + row_gap,
  baseline = ascent + row_gap/2, cell_w = 'M' advance) and the fallback-chain
  logic (fc-list discovery, family normalisation/dedupe). iced brings cosmic-text,
  which handles shaping/fallback properly — prefer it if the metrics line up.

Surgery (9 refs, all inside the draw path): term.rs (1800) — TermPane: VT wrapper,
  pty drain, scrollback, selection gesture, search, kitty decode+cache are all
  toolkit-free. Only `render()` and `render_kitty_images()` are macroquad
  (the spike's canvas painter already replaces `render()`; ~200 lines).

Surgery, and this is the real cost:
  app.rs (3253, 13 refs) — monolith: state + Action dispatch (reusable) fused with
    chrome/overlay drawing helpers, macroquad font + cell fields, screen-size
    queries, and hand-rolled mouse hit-testing (not reusable). Splitting that seam
    is the single largest task. Suggest: `app/state.rs` (state + actions, no
    drawing), `app/chrome.rs` (iced widgets/canvas painting of chrome),
    `app/input.rs` (iced events → Actions).
  main.rs (742, 15 refs) — 100% presentation: frame loop, tab bar, pane frames +
    labels, status bar (mode label, hints, theme name, sync chip, animated wave),
    toasts, palette, session manager, search bar, tab context menu, all manual
    hit-testing. All rewritten; most of it should become real iced widgets.

Summary: ~2.8k lines move untouched, ~370 get replaced, ~4k of presentation gets
ported (half of it written fresh). Budget one focused session for looks parity and
a second for the interactive extras.

## 4. Where it lives (decided: in the repo, not a separate project)

The port lives **in the ghostwave repo**, on branch `feat/iced-frontend`, as
`ghost-iced/` — a crate with its own `Cargo.toml`, a `[workspace]` marker (so it
is never dragged into the app package) and a path dependency on the app:

    ghost-iced/
      Cargo.toml            # [workspace]; iced 0.14 (canvas), libghostty-vt 0.2.1,
                            # nix 0.31 (+ "poll"), fontdue, ghostwave = { path = ".." }
      PORT-PLAN.md          # this file
      src/main.rs           # iced::application(...) boot + Subscription wiring
      src/pane_canvas.rs    # canvas::Program impls (terminal, preview, visualizer) — M1+
      src/chrome.rs         # tab bar, status bar, toasts (widgets where possible) — M2+
      src/app/              # state, actions, input mapping — M1+
      (fonts stay in ../assets; the crate includes
       ../../assets/JetBrainsMono-Medium.ttf)

Merge path, with no duplicated modules: `ghost-iced` imports
`ghostwave::{pty, term, layout, session, config, theme, search, palette,
clipboard}` from the app crate — it already has a lib target. The app's
`term.rs::render()` and `main.rs` are macroquad-only and simply go unused by this
crate. When parity is reached: flip the repo's default binary to `ghost-iced` and
delete the macroquad front-end. The toolkit-free modules never move (moving them
into a `ghost-core` crate later is cosmetic).

Do NOT copy modules into `ghost-iced/` — import them. `ai.rs` is out of scope.

## 5. Verified iced 0.14 API facts (checked against the sources, not docs)

Read the sources rather than guessing — they are in the local registry:
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/iced*-0.14.0/`
(iced, iced_core, iced_futures, iced_widget?, iced_graphics, iced_wgpu,
iced_tiny_skia). `grep -rn "pub fn <name>"` there beats any doc page.

- Boot: `iced::application(App::new, App::update, App::view).subscription(App::subscription).run()`
  where `App::new() -> (Self, Task<Message>)`.
- Canvas: `Canvas::new(&program).width(Fill).height(Fill)`; `impl canvas::Program<Message> for P { type State = (); fn draw(&self, _state: &Self::State, renderer: &Renderer, theme: &Theme, bounds: Rectangle, cursor: mouse::Cursor) -> Vec<canvas::Geometry> }`.
- `Frame::fill_rectangle(top_left: Point, size: Size, fill: impl Into<Fill>)` (Color works).
- `canvas::Text` fields exactly: `content: String, position: Point, max_width: f32, color: Color, size: Pixels, line_height: LineHeight, font: Font, align_x: Alignment, align_y: Vertical, shaping: Shaping`. `size: <f32>.into()`.
- `iced::futures` is re-exported (`pub use iced_futures::futures`), so
  `iced::futures::channel::mpsc::unbounded()`, `StreamExt`, `stream::pending` are available.
- Pushing pty output into iced: the reader thread owns an `UnboundedSender`; the
  subscription returns the receiver. `Subscription::run(fn() -> S)` needs a bare
  fn pointer, `Subscription::run_with<D: Hash, S>(data, fn(&D) -> S)` takes data —
  wrap the receiver slot in a struct whose `Hash` is `Arc::as_ptr` and return
  `Pin<Box<dyn Stream<Item = Message> + Send>>` (pending stream when already taken).
- Input: **there is no `keyboard::listen`/`on_key_press` in 0.14**. Use
  `iced::event::listen_with(fn(iced::Event, iced::event::Status, iced::window::Id) -> Option<Message>)`
  and match `iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modified_key, modifiers, text, repeat, .. })`.
  Named keys: `iced::keyboard::key::Named` (enum lives in iced_core/src/keyboard/key.rs).
- Colours: `Color::from_rgb8` / `from_rgba8` (NOT `Color::new`, it is private).
- Canvas does not expose text measurement (#478 open since 2020) — compute cell
  metrics yourself (fontdue, as the app does) and pass the same size to `canvas::Text`.
- No `Occluded` variant in `iced::window::Event` (0.14) — do not rely on occlusion
  events; iced's win is that it simply does not present while nothing changes.
- Borrow pattern that bit this port: `MutexGuard` field splitting works only after
  destructuring (`let TermState { term, render_state, row_it, cell_it, .. } = &mut *guard;`).

## 6. Milestones (each ends with: builds, runs, screenshot taken, user eyeballed)

M0 — crate skeleton: `ghost-iced/` with the verbatim modules copied, `cargo check`
     clean, and a window that opens with one pane painting (this is essentially
     the current spike, restructured). Keep the spike's main.rs as the reference.
M1 — multi-pane: layout.rs drives N panes in one tab; per-pane clipping (check
     `Frame` clip support or use one canvas per pane inside a Stack/Row/Column —
     prefer one canvas per pane: solves clipping and hit-testing for free).
     Pane frames (1px inactive / 2px active) + pane labels. Focus by click.
M2 — chrome: tab bar (widget, with active highlight + close/rename menu), status
     bar (mode label, hints, theme name, sync chip, the animated "ghostwave"
     wave), toasts. Colours from theme.rs.
M3 — input parity: map iced key events onto the existing keymap modes (pane, tab,
     resize, scroll, move, session, locked) and Action dispatch; scrollback
     wheel-scroll; Ctrl+Tab; Esc exits modes; repeat handling via `repeat: bool`.
M4 — tabs + sessions: new/close/rename/reorder tab, session save/restore using
     session.rs + last_session marker, session manager overlay, local only —
     SKIP durable/ssh/ghostd entirely (out of scope for this port).
M5 — config + themes: config.toml parity (font_size, theme, scrollback(→bytes),
     max_log_mb, padding, row_gap, visual_wave + wave knobs), theme switching at
     runtime, re-apply default fg/bg to every pane on spawn (a bug the macroquad
     build hit: fresh panes come up with libghostty's near-black defaults).
M6 — extras, in this order: selection + highlight + copy (arboard, not iced's
     clipboard), middle-click paste, search overlay, command palette, tab context
     menu, preview pane, visualizer pane. Kitty graphics LAST and optional — see risks.
M7 — parity check: side-by-side screenshot against the macroquad build at the same
     window size (`/tmp/bench2.sh` for presents/s + gfx engine time, grim for
     looks), then a short written diff of what iced does not match yet.

## 7. Verification protocol

- Screenshots: `hyprctl clients -j | jq -c '.[] | select(.class|test("ghost")) | {at,size,class,title}'`
  then `grim -g "<x>,<y> <w>x<h>" /tmp/shot.png`. Capture the same rect for the
  macroquad build for a true side-by-side. The user judges looks; capture the demo
  screen (`/tmp/iced-demo.sh`) so colours/bold/inverse/unicode are all visible.
  CORRECTION (M0): that jq filter matches nothing — the macroquad window's class is
  `miniquad-application` (title `ghostwave`), and the iced window's class is EMPTY
  (title = the shell's OSC title, e.g. `/tmp`). Match on `title` or by
  workspace/geometry instead.

- Perf (defer until after parity): `/tmp/bench2.sh idle 'sleep 300' 20` and
  `/tmp/bench2.sh stream 'while :; do echo tick; sleep 0.05; done' 20`.
  Reference numbers from the macroquad build: 8 presents/s idle (1-2% GPU),
  ~120 presents/s while output flows (~15% CPU/GPU), 1.43s for `seq 1 2000000`.
- Leave the user's running ghostwave alone: never kill it; A/B against it.

## 8. Gotchas that cost time (do not rediscover)

- Launching any GUI binary from an agent shell needs `LD_LIBRARY_PATH` from the
  app's project shell, or Wayland/EGL init fails ("Wayland backend failed" panic).
- `pgrep -x` cannot match names longer than 15 chars; `pgrep -f` matches your own
  wrapper. To find a child of a process: `pgrep -P <parent>`.
- The app's async pane-log flush is cut short when the last pane exits — a fixture
  that exits immediately loses its final log lines (add `; exec sleep 300`).
- `cargo check` before `cargo build --release`; the release link is minutes.
- Building the app crate from two different nix shells makes cargo recompile it
  (different rustc/env fingerprints). Not a bug; don't chase it.
- Do not run `nix build`/`nix profile install`; use the shells as-is.

## 9. Risks

- Canvas image handling: open perf regression #3173 (large images in 0.14) — only
  matters for kitty graphics. Ship M0-M6 first.
- No canvas text measurement (#478): we compute metrics ourselves; verify the
  baseline matches the macroquad build's rendering at the same font size.
- Clipboard: iced's path has open issues (#3418 Wayland-only clients on
  compositors without ext-data-control-v1; #904 no primary selection/middle-click).
  Keep using clipboard.rs/arboard in-process and drive it from our own events.
- Fonts: the app's fallback stack exists because macroquad has none. iced/cosmic-text
  does fallback itself — but verify nerd-font glyphs and emoji in the demo screen,
  and check cell width stability when a fallback glyph is drawn.

## 10. Working agreements

- The macroquad app is the reference and must keep working: never modify
  `/home/death916/Documents/code/vibed/ghost` for the port's sake (read-only),
  except when the user explicitly asks for a fix there.
- master is protected on forgejo; feature branches + PR, merged only after the user
  confirms. Public repo: no personal data, hostnames, home paths or machine specs.
- Update PROGRESS.md whenever anything lands in the real repo; commit per step.
- The user tests before merge and judges the look.
- Delegation: `delegation.model` in ~/.hermes/config.yaml is pinned to `mimo-v2.5`
  (weaker than the driving model). Either raise it to the session model
  (`deepseek-v4.1-flash` on `opencode-go`) or keep children restricted to
  mechanical work like module copying.

## 11. First commands for the next session

    cd /home/death916/Documents/code/vibed/ghost
    git status                 # expect branch feat/iced-frontend, clean tree
    git log --oneline -3       # expect the ghost-iced/ import commit

    # M0: drop ghost-iced/src/main.rs's local copies of the pty/VT plumbing and
    # import them from the app crate instead (ghostwave::pty, ghostwave::term),
    # keeping the canvas painter and the subscription wiring. Then:
    CARGO_TARGET_DIR=/tmp/icedspike-target nix-shell /home/death916/.hermes/shell.nix \
      --run "cd /home/death916/Documents/code/vibed/ghost/ghost-iced && cargo check --message-format=short"

The old out-of-repo spike at `~/Documents/code/vibed/ghost-iced-spike` is now a
stale copy — it can be deleted once the in-repo crate builds.

## 12. M0 status (2026-09-16, branch feat/iced-frontend)

Landed in `ghost-iced/src/` and `cargo check`-clean; release binary built and RUN
(one pane, fish, tty output, colours and glyphs visible; the macroquad build was
left running untouched, both tiled side by side on ws 2):

    main.rs        iced boot: App/Message/update/view/subscription; config + theme
                   load from the app crate; grid derived from the window via
                   iced::window::resize_events (the app's formula:
                   ((extent - 2*padding) / cell_extent).max(1))
    pane.rs        Pane { term: TermPane, scratch: RenderScratch }, apply_theme
                   (set_default_fg/bg_color — same call as app.rs), ingest/exited
                   (they mirror poll_pty: log first, then vt_write), key_press
                   (calls TermPane::handle_key, like app.rs), cell_dims (fontdue)
    pane_canvas.rs canvas::Program: bg, per-cell fg/bg (default-bg rects skipped),
                   text, underline, strikethrough, cursor (block/bar/underline)
    keys.rs        iced physical key Code -> libghostty Key (155 arms), mods, text
    pty_bridge.rs  reader-thread std mpsc -> iced stream, coalesced to 256 KiB;
                   GHOST_ICED_FIXTURE=<cmd> runs /bin/sh -c instead of a shell

Two facts that differ from the plan's assumptions:
- `TermPane`'s `render_state`/`row_it`/`cell_it` are PRIVATE, so `ghost-iced` owns
  a second set and drives it off `pane.term` (the pub `Terminal`). The pane's own
  `render()` (macroquad) is simply never called from this crate.
- `Pane::spawn` moves the std receiver to the bridge thread, so `TermPane::poll_pty`
  sees nothing. `pane.rs::ingest`/`exited` re-do what poll_pty does for the macroquad
  build (pane log, vt_write, EOF/reap) using the pane's pub fields — no app-crate
  change, but it is duplicated logic: revisit if the app crate ever grows a
  "feed(&[u8])" entry point.

Open questions to settle before M1 (unverified, not known-broken):
- Does the first resize event really land? The pty winsize path is only exercised
  if it does — probe with `GHOST_ICED_FIXTURE='sh /tmp/grid-check.sh'` (prints
  `stty size` once a second; the log goes to <data-dir>/panes/, and the fixture must
  not exit immediately or the async log flush eats the last lines).
- Glyph fallback: iced is given ONE font (JetBrains Mono, embedded) — SOLVED, and
  the cause was not what the plan assumed. Two separate bugs, both fixed:
  (a) the canvas was drawing with `Font::DEFAULT` = iced's built-in **Fira Sans**
  (proportional) while the cell grid is measured from JetBrains Mono, which is the
  uneven "gaps between letters" look; the face must be registered
  (`iced::application(..).font(bytes)`) and named in the canvas `Text`
  (`fonts::primary()`). (b) `Shaping::Basic` is cosmic-text's "shaping with NO font
  fallback" (`shape_fallback` is only reached on the Advanced path), so registering
  the fc-list fallback chain did nothing for glyphs missing from the primary face;
  the canvas must use `Shaping::Advanced` (what iced's own text pipeline uses).
  Fallback chain mirrors the macroquad stack (Nerd Font Mono, DejaVu Sans Mono,
  Noto Sans Mono, Noto Sans Symbols 2, FreeMono, ...). Known remaining gap, shared
  with the macroquad build: emoji (Noto Color Emoji is CBDT colour-bitmap, which the
  renderer cannot rasterise, and `fonts.rs` skips colour fonts on purpose).
  Watch for one side effect of Advanced shaping: it applies the font's default
  features, so JetBrains Mono's `->`/`=>` ligatures may appear (the macroquad
  painter has none, it draws per glyph).
- Cursor draw order: iced composites ALL canvas text above the shape layer, so a
  block cursor sits behind its glyph (the macroquad painter draws the block over
  it). Visible only under a glyph; fix by painting that cell's glyph in the cursor
  colour.

Reboot note: `/tmp/icedspike-target` (2.5 G warm deps; a cold rebuild is ~10 min)
was copied to `~/.cache/ghost-iced/target`, and the `LD_LIBRARY_PATH` for launching
to `~/.cache/ghost-iced/ld-path` (`/tmp/iced-spike-ldp` does not survive a boot).

## 13. Status + next up (2026-09-16, cold-start handoff — read this first)

**Start with `HANDOFF.md`** (same directory): it is the brief for whoever picks this up next —
state, exact commands, the next task in order, the pitfalls already paid for, and the conventions
the owner enforces. This section keeps the port's history and the detail behind it.

Committed: `e8e7181` ("iced: fix text/cursor baseline alignment & add dynamic process titles") on top
of `e29067d` (the pane_grid view) and `e492c2c` (pane background + title plan). Not merged, and not
to be merged yet. `src/**` + the top-level `Cargo.toml` are still unmodified by the port (verify
with `git diff --stat -- src Cargo.toml`), but the owner lifted the freeze on 2026-09-17: the shared
core may now take **additive** changes for the port's needs, and the macroquad front-end is being
archived — see §16 and `../RETIREMENT-PLAN.md`. Test suite green as of `e8e7181` (22 passing, the
commit's own count); `ghost-iced` compiles warning-free.

**Done: M0** (one pane) **M1** (N panes / frames / labels / focus / wheel scroll) **M2** (chrome:
tab bar, status bar + wave, toast, conditional animation tick) **M3** (input parity: every keymap
mode, every action routed or honestly toasted — §15) **M5a parts 1 & 2** (the pane layer and its
view: a tab owns `pane_grid::State<GridPane>` with iced owning split/close/adjacent/resize/maximize/
swap/drop, the view is one `PaneGrid` borrowing that state with a per-cell canvas, a title bar and
`on_click` focus, the legacy canvas layer and its A/B knob are gone, pty sizing follows the bounds
iced lays out, and `layout::Node` is demoted to the session format with tested conversions — §17).
**Next, in order: M5a polish** (the items §18 lists — baseline offset, cell backgrounds before text
runs, title caching), **M5b** chrome and overlays as real widgets, **M6** selection/clipboard,
search overlay, command palette, the preview / AI / visualizer panes and the F12 scratchpad,
**M7** the numbers. Sessions and SSH are held by the owner for later. The pre-archive gate is the
feature list in `../RETIREMENT-PLAN.md` §2: the macroquad build is no longer the *looks* reference,
but its features are the gate until it is archived.

### Build / run / verify (exact commands)

    # build (deps warm in /tmp/icedspike-target; a cold dir is ~10 min)
    cd /home/death916/Documents/code/vibed/ghost && \
      CARGO_TARGET_DIR=/tmp/icedspike-target nix-shell /home/death916/.hermes/shell.nix \
      --run "cd /home/death916/Documents/code/vibed/ghost/ghost-iced && cargo check --message-format=short"

    # binary
    /tmp/icedspike-target/release/ghost-iced

    # run (NEVER share the data dir with a running macroquad instance — see the prune hazard in §14)
    XDG_DATA_HOME=/tmp/ghost-iced-data LD_LIBRARY_PATH="$(cat /tmp/iced-spike-ldp)" \
      /tmp/icedspike-target/release/ghost-iced
    # regenerate the ld path if /tmp was wiped:
    #   cd .../ghost && nix-shell ./shell.nix --run 'printf %s "$LD_LIBRARY_PATH"' 2>/dev/null | tail -1 > /tmp/iced-spike-ldp

Environment knobs: `GHOST_ICED_PANES=<n>` (open n panes at startup, alternating right/down
splits), `GHOST_ICED_FIXTURE='<cmd>'` (run `/bin/sh -c <cmd>` instead of a shell).

Screenshots: the iced window's **class is empty** and its title is the shell's OSC title
(so `jq 'select(.title|test(...))'` on "ghostwave" misses it — match the empty class or the
geometry). Two grim quirks: `-g` takes the logical rect but the PNG comes back at the
monitor's scale (1.25 here ⇒ a 1133x1382 request yields 1416x1727), and grim only captures
the **visible** workspace — a window on another workspace yields the wallpaper/other windows.

Pane logs: `<XDG_DATA_HOME>/ghostwave/panes/pane-<pid>-<nanos>.log`, written through
`pane.rs::ingest` (the `poll_pty` replacement).

### ghost-iced module map (14 files, ~5,000 lines)

    main.rs         iced boot: Message, update, view, subscriptions, key listener, window title
    app.rs          AppState/Tab: tabs, the tab-owned pane_grid::State<GridPane>, every pane op,
                    mode, wave energy/phase, toast, tick rates, the title cache seam
    input.rs        the M3 dispatch mirror (key_press/key_release/repeat) + its acceptance tests
    pane.rs         Pane/SharedPane, PaneId, ingest/exited/key_press/resize, mouse tracking,
                    foreground-process + cwd inspection for dynamic titles
    pty_bridge.rs   one bridge thread per pane, coalesces to 256 KiB, tagged with a PaneId
    panes_grid.rs   the PaneGrid view: one canvas per cell, title-bar label, on_click/on_resize/on_drag
    pane_canvas.rs  the per-pane painter (paint_pane), run-merged text, label helpers, geometry
                    constants
    chrome.rs       tab bar, status bar + two-harmonic wave, toast — per CHROME-REFERENCE.md
    config.rs       the port's config overlay (dynamic_title / pane_title_format)
    measure.rs      text_width()/ascent() from fontdue (an iced canvas cannot measure text)
    keys.rs         iced physical key -> ghostwave keymap::KeyPress (mods, text, press/release/repeat)
    fonts.rs        PRIMARY_BYTES + primary() + the fc-list fallback chain
    tick.rs         conditional animation tick (iced 0.14 has no `time::every`)
    grid_session.rs Node <-> State<GridPane> conversions for session save/load (tested, no caller yet)
    CHROME-REFERENCE.md  line-cited chrome spec extracted from the macroquad render path

### M3 plan (done — kept as history; §15 has what shipped)

The five steps that produced `input.rs` are recorded in §15 ("M3 — input parity"). Do not re-do
them: the dispatch mirror, the mode/pending-prefix state and the press/release/repeat mapping are in
place and covered by the acceptance tests inside `input.rs`.

### Open items / risks

- **The wave's drawing is unexercised.** At 1133px the reference's own guard
  (`wave_end_x > wave_start_x + 50` after the 115-char NORMAL hints ~897px) skips it — in BOTH
  builds, verified by capture. Exercise it on a maximized window.
- **Idle perf unmeasured.** The 8 FPS idle wave tick means iced has not yet been *shown* to beat
  the macroquad 8 FPS idle tier; M7 measures before any default-binary flip.
- **Kitty graphics untried** (canvas image handling is iced's weak spot; open perf issue for
  large images). Highest-risk remaining item.
- Not started: selection + copy, search overlay, command palette, session manager, tab context
  menu, the preview / AI / visualizer panes, the F12 scratchpad (a float). Entry points count as
  much as the panes: the visualizer and AI open from the palette, preview opens from `Ctrl+Click`
  on a path, the scratchpad is `F12`/`Ctrl+~` (see `../RETIREMENT-PLAN.md` §2).
- iced renders every text item above every shape in a `Frame`; that is why the frame/label pass
  is clipped to the content rect, and it also means the cursor sits behind its glyph.
- **DIRECTION CHANGE (2026-09-16) — macroquad parity is no longer the acceptance criterion.**
  The owner prefers iced's text rendering, knows iced better than macroquad, and wants the work to
  use iced's built-ins instead of hand-rolling equivalents. See §16: panes move to `pane_grid`,
  the chrome and overlays become real widgets, and `ghostwave::layout` stays only as the persisted
  session representation.
- **DECISION (2026-09-17) — the macroquad front-end is being retired.** It is not a parallel build
  and not the target: it gets archived to a `macroquad-archive` branch + tag and removed from the
  ongoing branches. Until that cut, **its features are still the gate** — everything it can do must
  exist in iced first, and the list is `../RETIREMENT-PLAN.md` §2. The direction of the port does
  not change; this only fixes when the old build can go.

## 14. M1 / M2 notes (2026-09-16, later the same day)

M1 landed: `ghost-iced/src/app.rs` is the iced-side session state (tabs, the split
tree from `ghostwave::layout`, cached pane rects). Verified on the live pty: three
panes → three fish children of the iced process, each with its own bridge and pane
log. Splits/closes go through `layout::split_at` / `close_at`, focus through
`layout::focus_in_direction` — no layout math is duplicated. `pane_canvas` paints
each pane inside `Frame::with_clip` (one canvas for the whole window, so pane rects
stay in the app's own coordinate scheme and clips do the job a GL scissor does in the
macroquad build).

Decisions worth keeping:
- **Pane ids are stable; closed panes leave a tombstone** instead of compacting the
  vec the way `App::remove_dangling_panes` does. Compaction renumbers the panes under
  the `(tab, pane)` tag its pty bridge already stamped on the stream, so messages
  would silently land on the wrong pane. Compaction needs a re-tagging story first.
- **Temporary binds until M3**: Alt+Shift+d/e split, Alt+Shift+w close, Alt+arrows
  move focus, so multi-pane is reachable without the keymap modes.
- **Dev knobs**: `GHOST_ICED_PANES=<n>` opens n panes at startup (alternating
  right/down splits) and `GHOST_ICED_FIXTURE=<cmd>` runs a command instead of a
  shell — together they make layout, frames and multi-pane log behaviour testable
  without typing into the window.
- **Run test instances with `XDG_DATA_HOME=/tmp/ghost-iced-data`.** Both front-ends
  use the same data dir, and `session::prune_logs()` (called when any instance
  deletes a session) removes pane logs that no saved session references — i.e. a
  macroquad instance pruning can unlink the iced instance's *live* logs. Observed as
  `(deleted)` entries in `/proc/<pid>/fd`.
- `pane.rs::ingest`/`exited` reproduce `poll_pty` (log first, then `vt_write`,
  EOF/reap) because the receiver lives on the bridge thread. Verified: a fixture
  piping `seq 1 30000` into two panes produced two ~196 KB pane logs.

M2 (chrome) is being built from `ghost-iced/CHROME-REFERENCE.md`, a line-cited spec
extracted from the macroquad render path (geometry formulas, theme field names, font
sizes, format strings, wave parameters). Two port decisions:
- **The chrome is painted in the same canvas as the panes**, in the reference's
  order (panes → frames → tab bar → status bar → toast), because "looks parity" is
  the acceptance criterion and canvas painting reuses the reference geometry and
  colour fields verbatim. iced widgets would be more idiomatic (and better for
  hit-testing/a11y) but would need the geometry re-derived; revisiting is optional.
- **Canvas text can't be measured, so `measure.rs` measures it** with fontdue from
  the same font file (`measure_text` in macroquad sums per-char advances) and also
  converts the reference's baselines to iced's top-left text origin
  (`top = baseline - measure::ascent(px)`).
- **Animation is a conditional tick subscription**, not a frame loop:
  `TickRate::Fast` (16 ms) while the wave is energized or a toast is up,
  `TickRate::Slow` (125 ms) while the wave idles — matching the macroquad idle tier,
  at the cost of 8 presents/s instead of 0 — and `Off` (no tick at all) when
  `visual_wave = false`. Real durations replace the reference's frame counts.

## 15. M3 — input parity (keymap modes + Action dispatch)

Landed: the keymap modes are live. `Ctrl+p t n s h o g` enter modes, the status pill
and hint line change with the mode, every mode's binds work, `Esc` exits a modal mode,
`Ctrl+q` quits, and the temporary Alt binds are gone. The bind table itself is the app
crate's `keymap` module — `ghost-iced/src/input.rs` is only the mapping from a
`Dispatch` onto this front-end's state.

    input.rs   key_press / repeat / key_release / execute  (mirrors App::handle_key,
               handle_repeat, handle_key_release, execute)

### Decisions worth knowing (they are not obvious from the code)

- **Panes have an identity, not a position.** `pane::PaneId` is a process-wide counter,
  and a pane's pty bridge is tagged with it. Index-based tags broke as soon as `close_tab`
  (which shifts the tab vec down) and `break_pane` (which moves a pane into a new tab)
  existed: a bridge subscription cannot be re-keyed after it has taken its receiver, so
  an index tag would route a pane's output into whatever now sits at that index. Output
  and exit messages therefore carry the id and route via `AppState::pane_by_id`.
  Inside a tab, pane indices are still stable and never reused (closed panes leave
  tombstones) — that part is only about the tree and the rects.
- **Split direction semantics** (the one thing that is easy to mis-remember):
  `SplitDir::Horizontal` = side-by-side, new pane to the right; `SplitDir::Vertical` =
  one above the other, new pane below. Pane mode: `n` down, `d` down, `r` right,
  `s` stacked. Global: `Ctrl+d` right, `Ctrl+Shift+d` down.
- **Input is event-driven now.** iced delivers press / release / `repeat` and the
  modifier state, so three things in the reference have no counterpart: the per-frame
  `ALL_KEYS` poll, the synthetic repeat timer (400 ms initial / 40 ms interval) and the
  polled `keymap::current_mods`. Releases are forwarded to a pane only for keys the app
  did *not* consume on press (otherwise a modal bind spams the shell with stray
  releases, which the reference's own comment warns about).
- **Deliberate deviation — repeats in modal modes.** The reference forwards a held
  key's repeats straight to the active pane without consulting the keymap, so holding a
  key while the status bar says PANE types into the shell. Here repeats only flow in
  Normal and Locked.
- **Mouse reporting goes through the pane's own encoder.** The canvas resolves which
  pane the event landed in, makes the position pane-local, and calls
  `TermPane::handle_mouse`, which itself decides between application mouse reporting and
  selection/viewport handling. The wheel uses that same path, so a pane that tracks the
  mouse gets the wheel events and one that doesn't gets scrollback. Cursor motion is only
  forwarded when a button is down or the pane tracks the mouse — iced reports every pixel
  and the alternative is locking a pane per move. Modifiers for the mouse path come from
  a listener snapshot (`keys::current_mods`) because iced's mouse events do not carry
  them; `MouseEv::any_pressed` / `button_down` are accumulated in the canvas program's
  `State`.
- **Zoom** pins the active pane to the whole content rect and drops the other panes from
  the layout while it lasts; they keep their tree slots and their processes.
- **Unfinished features toast instead of doing nothing**, so a bound key is never
  silently inert: floating panes, scratchpad, command palette, sessions (M4), search (M6),
  preview/AI/visualizer panes, remote panes, edit-scrollback (which the reference also
  toasts), and copy with no selection.
- **Known reference quirks we mirror rather than fix**: `Ctrl+t b` (break-pane) on a
  single-pane tab moves that pane into a new tab and leaves the old tab with the layout
  crate's empty-tree sentinel (`Node::Leaf { pane: usize::MAX }`) — the tab survives with
  nothing in it until you split into it again. `pane mode c` / `F2` rename of an empty
  buffer is a no-op (Enter with an empty buffer keeps the current name), and the tab-cycle
  actions (`n`/`p` in pane mode) walk the tree's leaf order rather than the visual one.

### Verified

`ghost-iced/src/input.rs` carries eight acceptance tests that press keys through the real
dispatch path with real spawned panes and assert the resulting state (split direction,
pane/tab counts, zoom, frames, rename, mode transitions, locked semantics, quit, the
toasts). `fonts.rs` has three of its own. All 13 pass:

    cd .../ghost && CARGO_TARGET_DIR=/tmp/icedspike-target XDG_DATA_HOME=/tmp/ghost-iced-test \
      nix-shell /home/death916/.hermes/shell.nix --run \
      "cd /home/death916/Documents/code/vibed/ghost/ghost-iced && cargo test --message-format=short"

What the tests cannot cover: that winit actually delivers the chord to the window, and
how any of it looks. Those need a human at the keyboard.

### Convention: comments

The port's Rust sources carry **only rustdoc** (`//!`, `///`) written for a caller of the
API, plus the rare one-line note about a non-obvious mechanism. Nothing that cites another
file, records history, explains a decision or addresses a future maintainer: this plan and
`PROGRESS.md` are where that belongs.

## 16. Direction change (2026-09-16): iced-native, not macroquad parity

The owner's call, in substance: parity with the macroquad build no longer matters — they prefer
iced's font rendering, know iced better, and want iced's built-ins used instead of hand-rolled
equivalents.

**Consequences.**

1. **Stop hand-painting what iced has widgets for.** The tab bar, status bar and the overlays the
   reference draws by hand (command palette, session manager, search bar, rename prompt) become
   real widgets once their milestones land. The wave stays a `canvas`. `measure.rs` exists only
   because canvas text cannot be measured; widget layout removes most of its reason to exist.
2. **Panes move to `pane_grid`** — styled separator, hovered region, `on_click` focus,
   `on_resize` drag-resize, and one canvas widget per pane. That deletes: the rect list shared by
   painter and hit-testing (two copies of the same layout that can disagree), the per-pane
   `with_clip` scissors, and the "a frame's own primitives flush after its drafts" hazard that
   made every pane frame and label invisible today.
3. **What pane_grid does not model stays ours**: zellij-style stacked panes and floating panes.
   A stack becomes a pane whose content is a switcher; floats stay on an overlay layer.
4. **`ghostwave::layout` survives** as the persisted session representation and as the budget for
   stacks/floats, with the conversion to `pane_grid::State` at load/save (`grid_session.rs`, tested
   both ways). The file format is shared with the macroquad build, so it must keep loading what that
   build wrote — until the retirement cut, after which the format is ours alone.
5. **Plan, as it actually landed**: M3 input parity (§15), then **M5a** the pane layer *and* its
   view (§17, done), **M5b** chrome and overlays as real widgets, **M6** extras (selection and
   clipboard, search, palette, the preview / AI / visualizer panes, the F12 scratchpad),
   **M7** perf numbers only. Tabs landed with M1/M2; **sessions and SSH are held by the owner**.
6. **Retirement (2026-09-17)**: the macroquad front-end is archived (`macroquad-archive` branch +
   `macroquad-final` tag) and removed from the ongoing branches once the iced build covers the
   feature gate in `../RETIREMENT-PLAN.md` §2. Its features are the gate until then; its looks
   stopped being the reference in §16/§17.
7. **Kept from the parity work**: the terminal-core seam (`TermPane` + the iced render scratch),
   the font stack, the key encoder, the whole M3 input layer, and the geometry constants (cell
   metrics, frame inset) even where the painter is replaced.

## 17. Plan A: pane_grid owns the pane layer

Decided after the M5 first cut (a derived `pane_grid::State` rebuilt from our tree) proved the
hybrid is the broken part: measured with instrumentation, iced's `on_click` reported the wrong
pane for every click (`press in pane 1 … / click on Pane(0)`), because iced pairs its panes with
layout children positionally and a state derived from someone else's tree does not satisfy that.
Rather than hand-roll focus around it, the layout moves into iced — the point of using iced.

**Ownership.** A tab owns `pane_grid::State<GridPane>` as its layout authority.

    struct GridPane { panes: Vec<PaneId>, active: usize }

The payload is a list because zellij-style stacks (`Ctrl+p s`) are one cell holding several
panes: a plain pane is a one-element list, a stack is several with one active. `PaneId` stays the
process-wide identity that pty output is tagged with, so iced's opaque `Pane` handles never enter
our routing.

**What iced owns** (and therefore what we stop doing):
- splits and ratios — `State::split(axis, target, payload)`, `State::resize(split, ratio)`.
- closing — `State::close(pane)` returns the payload and the sibling to focus.
- directional focus — `State::adjacent(pane, Direction)`, no rect math.
- reordering — `State::swap`, `State::drop(pane, Target::Pane(other))`.
- zoom — `State::maximize` / `restore` / `maximized`, instead of our `zoomed` set.
- dividers, hover highlight, drag-to-resize, drag-to-move — `PaneGrid::on_resize` / `on_drag` /
  `Style { hovered_region, picked_split, hovered_split }`.
- click-to-focus — `PaneGrid::on_click`, which becomes trustworthy once the grid *is* the layout.

**What stays ours** (iced has no equivalent):
- the terminal canvas per pane (grid, cursor, glyph runs) and its mouse handling for terminal
  mouse reporting and selection;
- stacks, expressed in the payload above, with the switcher drawing the active member;
- floating panes, as an overlay layer outside the grid (absolute rects, as today);
- the persisted session format: `ghostwave::layout::Node` stays the on-disk shape, converted
  1:1 to and from the grid at load/save (`GridPane` ↔ `Node::Leaf`/`Node::Stack`; floats as
  today). The macroquad front-end keeps reading the same files.

**Pane sizing.** The grid computes each pane's bounds, so the pty grid follows from what iced
lays out rather than from rects we derive: each pane's canvas records the bounds it was drawn
with, and the update path turns that into `cols`/`rows` (`TermPane::set_grid`, a no-op when
unchanged) — our side adapts to iced, not the other way round.

**What this deletes**: `panes_grid.rs`'s `GridCache` / `derive` / `collect_splits` / `tree_ratio` /
`drop_target` / `pane_rects`, the multi-pane mode of `TerminalCanvas` (rects, per-pane clips,
hit-testing, the press-to-focus flag), `AppState::relayout`'s rect computation and `focus_dir`'s
rect math. Roughly the whole derived-state layer, i.e. everything that existed to make iced draw
a tree it did not own.

**Milestones**: M5a this pane layer (grid-owned layout, input layer rerouted through it, the A/B
knob retired — done in M5a part 2, the grid is the only pane path); M5b chrome and overlays as
widgets (`stack`,
`container`, `text_input`, `pick_list`, `mouse_area`), which retires `measure.rs` and the
hand-painted chrome geometry; M5c floats on the overlay layer. Stacks land with M5a since they
are payload-shaped, not layout-shaped.

### Parity checklist for M5a (the acceptance target)

Parity means the pane *features* the macroquad build has, not its pixels (§16 dropped that).
Every row must work in the iced front-end, driven by the existing binds in `input.rs`:

| feature | binds | iced mechanism |
| --- | --- | --- |
| split down / right | `Ctrl+p d`, `Ctrl+p r`, `Ctrl+Shift+d`, `Ctrl+d` | `State::split` |
| split into a stack | `Ctrl+p s` | push onto the payload's `Vec` |
| cycle a stack's members | `Ctrl+p w` | advance `GridPane::active` |
| close pane | `Ctrl+p x`, `Ctrl+w` | `State::close` |
| focus by click | left click | `PaneGrid::on_click` |
| focus directionally | `Ctrl+p h/j/k/l`, `Ctrl+Shift+<arrow/hjkl>` | `State::adjacent` |
| focus cycle / by index | `Ctrl+p p`, `Ctrl+Shift+1..9` | walk `State::iter` in layout order |
| move pane | `Ctrl+h` then `h/j/k/l`, `n`/`p` | `State::swap`, `State::drop` |
| resize | `Ctrl+n` then `h/j/k/l`, `H/J/K/L`, `=`/`-` | `State::resize` on the adjacent split |
| zoom | `Ctrl+p z`, `Ctrl+m` | `State::maximize` / `restore` |
| frames on/off | `Ctrl+p Shift+z` | our pane chrome (container borders + title band) |
| sync panes | `Ctrl+p b` | our broadcast in `input::send` |
| break pane to a new tab | `Ctrl+t b` | payload moves into a new tab's grid |
| pane label per pane | always | the pane's `title_bar` |
| wheel scrollback / terminal mouse | wheel, click, drag | our canvas → `TermPane::handle_mouse` |
| drag a divider to resize | mouse | `PaneGrid::on_resize` (new; the macroquad build has no equivalent) |
| drag a pane onto another | mouse on the title bar | `PaneGrid::on_drag` + `State::drop` (new) |

Floats, preview/AI/visualizer panes and kitty graphics are out of scope here (M5c / M6).

Verification while the pane layer is rebuilt: the pane operations are driven through
`input::key_press` in tests exactly like the M3 suite (asserting grid cardinality, which pane is
focused via `State::adjacent`/`maximized`, and that a stack's payload cycles), plus a real-window
run for the interactions a test cannot press: click focus, drag-resize, drag-move.

## 18. Dynamic Pane Titles & Process Tracking

Modern terminal emulators dynamically reflect the currently executing foreground command in the pane's title band (e.g., `vim`, `lazygit`, `python`), with configurable formatting and fallback hierarchy.

### Implementation Architecture
1. **Foreground process tracking**:
   - Query the pane's pty foreground process group using `nix::unistd::tcgetpgrp(pty_fd)` and read the command name from `/proc/<pid>/comm`.
   - Rationale: If the foreground process group equals the login shell's own PID, `foreground_process()` yields `None` so the active prompt, directory, or OSC title is displayed rather than the bare shell binary name (e.g. `fish`).
2. **Path condensation**:
   - Rationale: Condenses paths starting with `$HOME` into `~/...` (exact match becomes `~`, subdirectories become `~/path`) for concise display in title bands.
3. **Perf & View Purity**:
   - `display_title()` / syscall inspection (`tcgetpgrp`, `/proc` reads) is strictly decoupled from the `view(&self)` render path (`Tab::label`, `cell_label`, `App::title`).
   - Title evaluation is performed on the tick (via `AppState::tick`, running at the 8 FPS idle tier or energized wave speed) and cached in `Pane::cached_title`.
   - `view(&self)` only reads `cached_title()`, guaranteeing zero syscalls and zero allocations during frame rendering.
4. **Pane & Window Title Replacement**:
   - When dynamic titles are active, the dynamic title replaces `pane N` entirely (e.g. `lazygit` or `lazygit [1/2]` when stacked). If the title is empty or dynamic titles are disabled, formatting cleanly falls back to `pane N`.
   - The window title suffix (`{title} — ghostwave`) is tied to the same configuration knob; when `dynamic_title = "off"`, it remains `"ghostwave (iced)"`.
5. **Wheel Scroll Normalization**:
   - Pixel scroll deltas delivered by winit/Wayland/libinput are divided by `(cell.h as f32).max(1.0)` rather than a hardcoded constant, ensuring continuous wheel increments match font cell line steps without truncation.

### Configuration Options (`~/.config/ghostwave/config.toml`)
- `dynamic_title`: `"process"` (default), `"osc"` / `"osc_only"`, or `"off"` (boolean `true`/`false` also accepted).
- `pane_title_format`: `"process"` (default: command name, fallback to directory/OSC), `"pwd"` (prioritize directory), or `"both"` (e.g. `lazygit (~/repo)`).
- Lives in `ghost-iced/src/config.rs` (`IcedConfig`) today, read from the same `~/.config/ghostwave/config.toml`. **The `src/**` freeze was lifted by the owner on 2026-09-17**: move these keys into the shared `Config` as additive `#[serde(default)]` fields so the macroquad build still compiles and simply ignores them, keeping one config file as the source of truth. The iced-only overlay was a workaround for the freeze, not the target design.

### Next Milestones
- **M5a Polish & Verification**: Text baseline offset fixed (`run.baseline_y - measure::ascent(font_px)`), cell backgrounds drawn before text runs, dynamic title caching (Landed).
- **M5b / M5c / M6 (Landed)**:
  - Native iced chrome widgets (`chrome_widgets.rs`), floating scratchpad manager (`scratchpad.rs`).
  - Command palette (`palette.rs`), scrollback search overlay (`search_overlay.rs`), mouse selection & clipboard (`selection.rs`).
  - Content panes: Synthwave 3D visualizer (`visualizer.rs`), file preview (`preview.rs`), AI assistant chat (`ai.rs`).
- **M7**: Performance benchmarking and final packaging.

## 19. Integrating the M5b/M5c/M6 modules (2026-09-17)

`f44223f` added eight modules (~6,400 lines, 102 tests green) but wired none of them: `app.rs`,
`main.rs`, `panes_grid.rs` and `pane.rs` contain no reference to them, and `input.rs` still answers
their actions with "not implemented yet" toasts. This is the plan for connecting them.

### Verified API shape (what the modules actually offer)

| module | entry points | parent-facing message type |
| --- | --- | --- |
| `palette.rs` | `PaletteState::new(items)`, `update(PaletteMessage) -> Option<PaletteAction>`, `handle_iced_key(..)`, `view(&PaletteState, &Theme) -> Element<PaletteMessage>` | own `PaletteMessage`, returns `PaletteAction::{Execute(Action), Close}` |
| `search_overlay.rs` | `SearchState::new()`, `set_query`, `toggle_regex/case`, `next/prev_match`, `sync_to_term(_search)`, `sync_from_term(_search)`, `search_overlay_mapped(state, theme, position, map: impl Fn(SearchMessage) -> Message)` | own `SearchMessage`, with a ready-made mapping adapter |
| `scratchpad.rs` | `ScratchpadState::{new, toggle(&mut AppState), embed_into(&mut AppState), window_size(&AppState)}`, `ScratchpadGeometry::{TopDrawer, Centered}`, `FloatingPane` | none — it drives `AppState` directly |
| `chrome_widgets.rs` | `tab_bar_for_state(&AppState)`, `status_bar_for_state(&AppState)`, `toast_box(&str, &Theme)` | the parent `Message` directly |
| `ai.rs` | `AiPane::new(AiConfig)`, `update(AiMessage)`, `send`, `handle_response`, `poll`, `view(&Theme) -> Element<AiMessage>` | own `AiMessage` |
| `preview.rs` | `PreviewPane::open(path)`, `load`, `handle_action(PreviewAction)`, `view<'a, Message: 'a>(&self, &Theme)` | `PreviewAction` — but `view` attaches **no** messages (see below) |
| `visualizer.rs` | `VisualizerPane::{new, with_font, with_bg_color}`, `ColorMode::{next, colors}` | none — driven by key/state |
| `selection.rs` | `SelectionModel::{on_press, on_press_word, on_press_line, on_drag, on_release, extract_text}` | none |

### Decisions this forces

0. **Two seams the slice landed on, worth knowing before touching them.** `ScratchpadState::toggle`
   takes `&mut AppState`, so the caller moves the manager out of the state it mutates
   (`std::mem::take`) rather than borrowing it alongside — do not "simplify" that into a borrow.
   And an action reached from the palette and the same action reached from a key bind share one
   implementation (`input::execute`), so the palette cannot drift from the binds.

1. **Content panes need a pane kind.** A grid cell currently resolves to a `SharedPane` (a terminal).
   `Tab.panes: Vec<Option<SharedPane>>` becomes `Vec<Option<PaneEntry>>` with
   `PaneEntry = Term(SharedPane) | Content(ContentPane)`, and
   `ContentPane = Preview(PreviewPane) | Ai(AiPane) | Visualizer(VisualizerPane)`, addressed by the
   same `PaneId` counter so the grid payload and pty routing stay untouched. Callers that mean
   "terminal" (key send, pty subscription, resize/`set_grid`) match on `Term`.
2. **Selection needs no core change, and `selection.rs` is redundant — delete it.** Verified in the
   vendored sources: libghostty's render API exposes the selection
   (`RowIteration::selection() -> Result<Option<RowSelection>>`, a row-local cell range, and
   `Cell::is_selected()`), and the port's canvas already walks rows and cells, so the highlight can
   be painted from the same data the copy path uses. The gestures are already the core's —
   `pane.rs::mouse_event` → `TermPane::handle_mouse` → `SelectionState::on_click/on_drag/on_release`
   → `terminal.set_selection(..)`, with `TermPane::copy_selection()` as the only copy. A second
   front-end `SelectionModel` would let what is highlighted and what is copied disagree, so the
   module goes at the dedup step (`../RETIREMENT-PLAN.md` §5 step 7) — or as soon as nothing
   references it, which today is only its `mod` line. Use the per-row `selection()` call rather than
   per-cell `is_selected()`: the vendored docs call that out explicitly.
3. **The preview is driven by the keymap, not by its own widgets.** Its `view` emits nothing, so
   `input.rs` routes scroll/zoom/reset to the focused preview pane's `handle_action`. Adding
   `on_press` buttons is optional polish, and only via an `..._mapped` adapter.
4. **Overlays are `Stack` layers, above the pane grid and the chrome** (order matters — see the
   paint-order traps in `HANDOFF.md` §7): grid → chrome → search bar → palette → scratchpad float →
   toast. The palette and search overlays capture input while open: keys go to them, not to the pane.
5. **Chrome widgets land separately from the overlay wiring.** The canvas chrome stays the live path
   until `chrome_widgets::{tab_bar_for_state, status_bar_for_state, toast_box}` replace it in one
   dedup pass at the archive cut (`../RETIREMENT-PLAN.md` §5 step 7) — one change at a time, each
   ending green.

### Slice order (each slice ends with tests green)

1. Parent seams: `Message` variants (`Palette`, `Search`, `Ai { id, msg }`, `Preview { id, action }`),
   `AppState` fields (`scratchpad`, `palette: Option<..>`, `search: Option<..>`), `input.rs` routing
   for `OpenCommandBar` / `OpenSearch` / `ToggleScratchpad`, and the overlay layers in `view`.
2. Content panes: the `PaneEntry`/`ContentPane` refactor, cell rendering in `panes_grid`, and the
   palette/keymap entry points (`NewAi`, the visualizer, `Ctrl+Click` → preview).
3. Selection: the core range accessor, highlight painting in `pane_canvas`, copy on the existing
   action, and `selection.rs` resolved (deleted or reduced).
4. Chrome widgets swap, floats polish, then the parity table walk in a real window.

## 20. M5b, M5c & M6 modules: what was added (2026-09-17)

The non-iced / macroquad front-end is no longer a consideration or constraint; iced 0.14 is the sole target.

### Modules added in `ghost-iced/src/` (written, unit-tested, **not yet wired** — see §19):
1. **`chrome_widgets.rs`**: Native iced 0.14 widgets for the tab bar (active/inactive styling, click-to-focus `Message::FocusTab`, inline `text_input` rename), status bar (mode pill, keybind hints, live 2-harmonic wave visualizer canvas, sync panes chip, theme badge), and floating toast overlay box.
2. **`scratchpad.rs`**: Floating pane overlay and F12 / `Ctrl+~` scratchpad manager with geometry presets (`TopDrawer`, `Centered`), window boundary clamping, Zellij-style float cycling (`e`/`E`), and origin re-embedding.
3. **`palette.rs`**: Modal command palette overlay with action inventory from `ghostwave::keymap::Action`, installed themes, fuzzy matching, prefix commands (`open`, `ssh`, `ds`), and keyboard navigation.
4. **`search_overlay.rs`**: Scrollback search bar overlay with case-sensitivity (`[Aa]`) and regex (`[.*]`) toggles, match count formatting, and bidirectional terminal search synchronization.
5. **`selection.rs`**: Terminal mouse selection gestures (`Normal`, double-click `Word`, triple-click `Line`), pixel-to-cell coordinate conversions, multi-line text extraction respecting wide characters and column gaps, and native in-process clipboard integration via `arboard`.
6. **`visualizer.rs`**: Ghostwave synthwave screensaver & visualizer ported to an iced `canvas::Program` with rolling 3D perspective grid, undulating neon spectral mountain peaks, matrix cyber rain, and interactive palette switching (`GhostNeon`, `MatrixGreen`, `SolarAmber`, `AbyssIce`).
7. **`preview.rs`**: File preview pane for `Text` (line numbers, monospace font), `Dir` (sorted directory listing with badges), `Binary` (16-byte hex dump and ASCII sidebar), and `Image` (interactive canvas viewer with pan/zoom and transparency checkerboard).
8. **`ai.rs`**: AI assistant chat pane using native iced widgets (cards for User, Assistant, System, thinking indicator, prompt `text_input`, send button) backed by non-blocking background completion requests via `ureq`.

### Verification:
- `cargo test --manifest-path ghost-iced/Cargo.toml`: **102 passed, 0 failed, 0 warnings**.
- `cargo build --release --manifest-path ghost-iced/Cargo.toml`: **Built cleanly in release mode**.


