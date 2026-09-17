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

## 13. M1 / M2 notes (2026-09-16, later the same day)

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

