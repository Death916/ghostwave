# ghostwave

A waveterm/zellij-style terminal multiplexer built on
[libghostty-vt](https://github.com/Uzaaft/libghostty-rs) (the Rust bindings
for the Ghostty terminal), presented by [iced](https://iced.rs), with
zellij-style keybindings.

Wayland-first, mouse-friendly, and built around durable sessions: sessions are
named and saved to disk, and one is restored on launch.

The front-end is iced. Until September 2026 it was a macroquad game-engine
front-end; that build is archived (see [Retired](#retired-the-macroquad-front-end))
and is the only implementation of a few things listed under
[Not in this build](#not-in-this-build).

## Features

- Tabs, split panes (right/down/stacked), zoom, resize and move via mode keys
- **CWD Auto-Inheritance (OSC 7 / `/proc/{pid}/cwd`)**: new splits and tabs inherit the active shell's live directory.
- **Clickable Links & Files (`Ctrl+Click`)**: `Ctrl+Click` on a web URL opens your browser via `xdg-open`; a local file path opens in a preview pane.
- **Synchronized Panes (Broadcast Input)**: `Ctrl+p b` (or `Ctrl+t s`) sends input to every pane in the tab at once, with a `[SYNC]` badge.
- **Scratchpad Drawer (`F12` / `Ctrl+~`)**: a floating terminal overlay that toggles visibility without killing what runs inside it.
- **Floating panes**: the float bind lifts the focused pane out of the layout and paints it above everything else; hiding keeps it running and drains its output while invisible, and cycling brings hidden floats back.
- **Named sessions**: `Ctrl+o` opens the session manager. Sessions are saved to disk on switch and on quit, the last one is restored on launch (all of its tabs and panes, with scrollback), and you can rename, delete or detach. Pane output is logged and capped (`max_log_mb`).
- **Interactive Ghostwave Visualizer**: rolling synthwave grid with neon terrain and matrix rain (`Ctrl+Shift+P` → `Ghostwave (Screensaver Visualizer)`).
- **AI chat pane** (`Ctrl+Shift+P` → `New AI Chat`): talks to any OpenAI-compatible endpoint configured under `[ai]`, from a worker thread so the UI stays live.
- **Living Reactive Status Waveform**: status-bar sine wave reacting to typing speed and terminal activity (`visual_wave` in `config.toml`).
- **Preview pane**: text and directory listings for the file path under `Ctrl+Click`.
- zellij-style modal keybinds (Esc exits any mode)
- Command palette (`Ctrl+Shift+P`), find-in-viewport (`Ctrl+Shift+F`), copy/paste,
  scroll wheel, mouse selection + click-to-focus
- Themes (tokyonight default), cursor styles, font fallback stack (nerd-font
  glyphs + emoji via system fonts)

## Keybinds

| Bind | Action |
|------|--------|
| `F12` / `Ctrl+~` | toggle dropdown scratchpad drawer |
| `Ctrl+Click` | open URL in browser or file path in preview pane |
| `Ctrl+p` | pane mode (n new, d down, r right, s stacked, x close, h/j/k/l focus, p cycle, f float, e hide float, E cycle float, b sync/broadcast, z frames, c rename) |
| `Ctrl+t` | tab mode (n new, d close, o next, p prev, r rename, s toggle sync) |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | next / previous tab |
| `Alt+n` | new split pane |
| `Alt+f` | toggle pane fullscreen within window |
| `Ctrl+n` | resize mode (h/j/k/l) |
| `Ctrl+s` | scroll mode (arrow keys; Ctrl+s exit) |
| `Ctrl+h` | move mode (h/j/k/l — sticky; Esc or Ctrl+h exits) |
| `Ctrl+o` | session mode (w session manager, d detach) |
| `Ctrl+g` | lock (passthrough) |
| `Ctrl+q` | quit (saves the session) |
| `Ctrl+M` | zoom pane (floats can't zoom — embed first) |
| `Ctrl+Shift+P` | command palette |
| `Ctrl+Shift+F` | find |
| `Ctrl+Shift+C` / `V` | copy / paste |
| right-click tab | tab menu: rename, close |

## Build & run

Requires: Rust (>= 1.90), Zig 0.15.2 (for the vendored ghostty build),
a Wayland session, and the usual dlopen-able GL/wayland/X11 libs.

```sh
nix-shell ./shell.nix     # dev shell with everything pinned (NixOS)
cargo build --release
cargo run
```

## Configuration

`~/.config/ghostwave/config.toml` (created with defaults on first run):

# Appearance
font_family = "JetBrains Mono" # optional system font name; defaults to bundled JetBrains Mono
# font_size is in points (12 = 12pt, like ghostty/waveterm)
font_size = 12.0
theme = "tokyonight"
scrollback = 10000          # terminal scrollback lines per pane
max_log_mb = 32             # per-pane session log cap (MiB), front-truncated
padding = 6.0
row_gap = 4.0
cursor_style = "block"      # "block", "bar", "beam", "underline", "hollow"

# Visual candy
visual_wave = true          # living reactive sine wave in status bar
wave_speed = 1.0            # wave animation speed multiplier
wave_amplitude = 1.0        # wave height multiplier

[ai]                        # AI chat pane: any OpenAI-compatible endpoint
base_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"
api_key_env = "OPENAI_API_KEY"
```

Session data (pane logs + session state) lives in
`~/.local/share/ghostwave/`.


## Project layout

One crate: `src/lib.rs` is the terminal core, `src/main.rs` and the modules
beside it are the iced front-end.

- `src/term.rs` — terminal core: libghostty-vt state, pty, input, selection, links
- `src/session.rs` — durable sessions (async pty log tee, save/restore, pruning)
- `src/layout.rs`, `src/keymap.rs`, `src/config.rs`, `src/theme.rs`, `src/search.rs`,
  `src/clipboard.rs`, `src/pty.rs` — core services shared by the front-end
- `src/main.rs` — iced application: window, message loop, layer stack
- `src/app.rs` — app state: tabs, panes, layout, floats
- `src/ui_session.rs` — named sessions wired into the front-end
- `src/pane.rs`, `src/pane_canvas.rs`, `src/panes_grid.rs` — panes and their painting
- `src/chrome_widgets.rs` — tab bar, status bar, toasts, built as widgets (`src/measure.rs` serves the pane painter)
- `src/input.rs`, `src/keys.rs` — key/mouse dispatch
- `src/scratchpad.rs` — the F12 scratchpad and the float list
- `src/visualizer.rs`, `src/ai.rs`, `src/preview.rs` — content panes
- `src/ui_fonts.rs` — font stack with fallback discovery (fc-list)
- `ghostd/` — the remote session daemon (standalone crate; see "Not in this build")
- `tests/term_core.rs` — terminal-core semantics (wrap/reflow)
- `PROGRESS.md` — development journal

## License

MIT (see `Cargo.toml`). The bundled font
(`assets/JetBrainsMono-Medium.ttf`) is licensed under the SIL Open Font
License 1.1 — see `assets/LICENSE-JetBrainsMono.txt`.
