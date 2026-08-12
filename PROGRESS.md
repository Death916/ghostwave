# ghostwave — build progress

waveterm-style terminal built on **libghostty-vt** (the Rust crate from
`Uzaaft/libghostty-rs`) + macroquad, with **zellij-style keybindings**.
Durable sessions deliberately skipped (zellij session mode shows a toast).

Project root: `/home/death916/Documents/code/vibed/ghost/`
Binary name: `ghostwave`

---

## STATUS: main terminal features solid — render/mouse/keybinds fixed, parity work in progress

### Recent work (commits 2571ca2 → HEAD)
- Render: always-redraw per libghostty-vt docs (ghostling-rs pattern);
  pane bg = terminal default from snapshot.colors(). Fixed blank/flicker.
- Mouse: drag selection only while left button held; click focuses pane.
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
  NOTE: must use PROJECT shell.nix (`nix-shell ./shell.nix`); the
  ~/.hermes shell.nix lacks these libs → runtime dlopen failure.
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
1. Re-verify scissor: pane draws no longer rely on skip; confirm
   set_scissor isn't clipping multi-pane layouts (test a split).
2. Screenshot QA with grim (window must exist first):
   `grim /tmp/ghostwave.png` then vision-check it.
3. ✅ DONE: theme → terminal default fg/bg wired
   (apply_terminal_colors; set_default_fg/bg_color on all panes).
   Verified: pane bg corner shifted ghostty-gray → tokyonight tint.
4. ✅ DONE: tab name follows active pane OSC title (unless renamed).
   NOTE: fish on this box emits NO OSC 0 titles (verified via script(1))
   → tab stays "Tab 1" until the shell sends one; mechanism is correct.
5. Test zellij binds live: Ctrl+p n/d/r/s/x/hjkl, Ctrl+t, Ctrl+n resize,
   Ctrl+s scroll, Ctrl+Shift+P palette, Ctrl+Shift+F search,
   Ctrl+Shift+C/V copy/paste, Ctrl+M zoom, Ctrl+W close.
6. cargo build --release for perf check (always-redraw may need
   per-row dirty optimization later; fine for now).

---

## Build / run commands (from project root — project shell.nix included)

    nix-shell ./shell.nix --run "cargo build 2>&1"
    nix-shell ./shell.nix --run "cargo run 2>&1"
    # or interactively:  nix-shell   then   cargo run

- `~/.hermes/shell.nix` was extended with: cargo, rustc 1.95, rustfmt,
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
  h/j/k/l+arrows focus, p cycle, f zoom, z frames, w cycle-stack,
  c rename, e/i toast
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

tabs, split panes (right/down/stacked), zoom (Ctrl+M / f), command palette
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

## External facts

- User's machine: ASUS S16, AMD AI 465 (RDNA 3.5 iGPU), 32GB, Hyprland
  Wayland session (WAYLAND_DISPLAY=wayland-1). wl-copy/wl-paste present.
- nixpkgs zig attr currently resolves to 0.16.0 (ghostty needs ≥0.15.2).
- rustc 1.95.0 in nixpkgs (crate needs ≥1.90).
- To screenshot for QA: grim/hyprctl may exist — check before claiming
  visual verification.
