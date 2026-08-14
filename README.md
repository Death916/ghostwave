# ghostwave

A waveterm/zellij-style terminal multiplexer built on
[libghostty-vt](https://github.com/Uzaaft/libghostty-rs) (the Rust bindings
for the Ghostty terminal) and [macroquad](https://macroquad.rs), with
zellij-style keybindings.

Wayland-first, mouse-friendly, and designed around durable sessions: every
session saves continuously (screen *and* scrollback), and switching
sessions parks the outgoing one's processes — they keep running in the
background until you switch back or delete the session.

## Features

- Tabs, split panes (right/down/stacked), zoom, resize via mode keys
- **Floating panes** (zellij): float any pane above the layout (pane-mode
  `f`), move/resize it (Ctrl+h / Ctrl+n), or hide it as a background pane
  (`e`) that keeps running and draining while invisible; `E` cycles floats
  and shows hidden ones again.
- **Durable sessions**: every pane's output is logged; the session manager
  (Ctrl+o → w) switches between named sessions, each restoring ALL of its
  tabs and panes with full scrollback on launch. Session state persists
  continuously, so even a crash loses at most a couple of seconds. Logs
  are capped (`max_log_mb`) and pruned.
- zellij-style modal keybinds (Esc exits any mode)
- Command palette (Ctrl+Shift+P), find-in-viewport (Ctrl+Shift+F),
  copy/paste (Ctrl+Shift+C/V), scroll wheel, mouse selection + click-to-focus
- File preview panes (text, images, directories), SSH connections
- Themes (tokyonight default), cursor blink, kitty graphics protocol,
  font fallback stack (nerd-font glyphs + emoji via system fonts)
- Native Wayland backend (miniquad); clipboard via wl-copy/wl-paste

## Keybinds

| Bind | Action |
|------|--------|
| `Ctrl+p` | pane mode (n new, d down, r right, s stacked, x close, h/j/k/l focus, p cycle, f float, e hide float, E cycle float, z frames, c rename) |
| `Ctrl+t` | tab mode (n new, d close, o next, p prev, r rename) |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | next / previous tab |
| `Ctrl+n` | resize mode (h/j/k/l) |
| `Ctrl+s` | scroll mode (arrow keys; Ctrl+s exit) |
| `Ctrl+h` | move mode (h/j/k/l — sticky; Esc or Ctrl+h exits) |
| `Ctrl+o` | session mode (w session manager, d detach) |
| `Ctrl+g` | lock (passthrough) |
| `Ctrl+q` | quit (saves session) |
| `Ctrl+M` | zoom pane (floats can't zoom — embed first) |
| `Ctrl+Shift+P` | command palette |
| `Ctrl+Shift+F` | find |
| `Ctrl+Shift+C` / `V` | copy / paste |
| `ds <host>` in palette | durable ssh pane (ghostd) |
| disconnected remote pane: `Enter` | connect / reconnect |
| disconnected remote pane: `k` | kill the remote session |
| disconnected remote pane: `Esc` | stop auto-reconnect |
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

```toml
# font_size is in points (12 = 12pt, like ghostty/waveterm)
font_size = 12.0
theme = "tokyonight"
scrollback = 10000          # terminal scrollback lines per pane
max_log_mb = 32             # per-pane session log cap (MiB), front-truncated
padding = 6.0
row_gap = 4.0

[remote]                    # durable remote sessions (ghostd)
auto_reconnect = true       # auto-retry a dropped remote pane
reconnect_attempts = 5      # before dropping to manual (Enter)
reconnect_delay_secs = 3    # initial retry delay (backoff caps at 15s)
```

Session data (pane logs + session state) lives in
`~/.local/share/ghostwave/`.

## Durable remote sessions (ghostd)

`ds <host>` in the command palette (`Ctrl+Shift+P`) opens a durable ssh
pane. Under the hood it runs `ssh -t <host> ghostd attach ghostwave-<host>`;
the ghostd daemon is uploaded to `~/.cache/ghostwave/` on the host on first
use (no install needed; both x86_64 and aarch64 binaries are embedded and
selected by the remote's `uname -m`).

- The remote session keeps running when the pane/ssh dies — long builds
  and shells survive disconnects, and output keeps being logged.
- Reconnecting (re-opening `ds <host>`, or pressing `Enter` on a
  disconnected pane) replays the full log — including everything printed
  while you were away — then resumes live.
- A dropped connection auto-reconnects (bounded: 5 attempts, 3s→15s
  backoff), then waits for `Enter`. `Esc` stops auto-reconnect, `k` kills
  the remote session (daemon cleans up socket/pid/log).
- Restoring a saved session restores remote panes DISCONNECTED — history
  is shown but nothing connects until you press `Enter`.
- OSC 52 copy (vim/tmux clipboard over ssh) is supported out of the box.
- `ghostd` builds: `nix build .#ghostd-x86_64 .#ghostd-aarch64`
  (static musl via nixpkgs; pure Rust, no zig).

## Project layout

- `src/term.rs` — terminal pane: libghostty-vt rendering, input, selection
- `src/session.rs` — durable sessions (pty log tee, restore, pruning)
- `src/keymap.rs` — zellij-style keybindings + modes
- `src/app.rs` — app state, tabs/panes/layout, mouse handling
- `src/fonts.rs` — font stack with fallback discovery (fc-list)
- `src/main.rs` — macroquad event loop + chrome rendering
- `ghostd/` — the remote session daemon (standalone crate)
- `tests/term_core.rs` — terminal-core semantics (wrap/reflow)
- `PROGRESS.md` — development journal

## License

MIT (see `Cargo.toml`). The bundled font
(`assets/JetBrainsMono-Medium.ttf`) is licensed under the SIL Open Font
License 1.1 — see `assets/LICENSE-JetBrainsMono.txt`.
