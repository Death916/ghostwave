# ghostwave

A waveterm/zellij-style terminal multiplexer built on
[libghostty-vt](https://github.com/Uzaaft/libghostty-rs) (the Rust bindings
for the Ghostty terminal) and [macroquad](https://macroquad.rs), with
zellij-style keybindings.

Wayland-first, mouse-friendly, and designed around durable sessions: pin a
tab and it comes back — screen *and* scrollback — after a restart.

## Features

- Tabs, split panes (right/down/stacked), zoom, drag-to-resize
- **Durable sessions**: every pane's pty output is tee'd to a log;
  right-click a tab → *Pin tab* → the tab (with full scrollback) is restored
  on next launch. Session state persists continuously, so even a crash loses
  at most a couple of seconds
- zellij-style modal keybinds (Esc exits any mode)
- Command palette (Ctrl+Shift+P), find-in-viewport (Ctrl+Shift+F),
  copy/paste (Ctrl+Shift+C/V), middle-scroll, mouse selection + click-to-focus
- File preview panes (text, images, directories), SSH connections,
  AI chat pane (any OpenAI-compatible endpoint)
- Themes (tokyonight default), cursor blink, kitty graphics protocol,
  font fallback stack (nerd-font glyphs + emoji via system fonts)
- Native Wayland backend (miniquad); clipboard via wl-copy/wl-paste

## Keybinds

| Bind | Action |
|------|--------|
| `Ctrl+p` | pane mode (n new, d down, r right, s stacked, x close, h/j/k/l move) |
| `Ctrl+t` | tab mode (n new, d close, o next, p prev, r rename) |
| `Ctrl+n` | resize mode (h/j/k/l) |
| `Ctrl+s` | scroll mode (arrow keys; Ctrl+s exit) |
| `Ctrl+h` | move focus mode (h/j/k/l) |
| `Ctrl+o` | session mode (currently a toast) |
| `Ctrl+g` | lock (passthrough) |
| `Ctrl+q` | quit (saves session) |
| `Ctrl+m` / `f` | zoom pane |
| `Ctrl+Shift+P` | command palette |
| `Ctrl+Shift+F` | find |
| `Ctrl+Shift+C` / `V` | copy / paste |
| `Ctrl+Shift+A` | AI chat pane |
| right-click tab | tab menu: pin/unpin (durable), rename, close |

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

```toml
# font_size is in points (12 = 12pt, like ghostty/waveterm)
font_size = 12.0
theme = "tokyonight"
scrollback = 10000
padding = 6.0
row_gap = 4.0

[ai]
base_url = "https://api.minimax.chat/v1"   # any OpenAI-compatible endpoint
model = "MiniMax-M2"
api_key_env = "MINIMAX_API_KEY"            # read from the environment
```

Session data (pane logs + session state) lives in
`~/.local/share/ghostwave/`.

## Project layout

- `src/term.rs` — terminal pane: libghostty-vt rendering, input, selection
- `src/session.rs` — durable sessions (pty log tee, restore, pruning)
- `src/keymap.rs` — zellij-style keybindings + modes
- `src/app.rs` — app state, tabs/panes/layout, mouse handling
- `src/fonts.rs` — font stack with fallback discovery (fc-list)
- `src/main.rs` — macroquad event loop + chrome rendering
- `PROGRESS.md` — development journal

## License

MIT (see `Cargo.toml`). The bundled font
(`assets/JetBrainsMono-Medium.ttf`) is licensed under the SIL Open Font
License 1.1 — see `assets/LICENSE-JetBrainsMono.txt`.
