# ghostwave chrome reference — macroquad → iced M2

Exact spec of the macroquad front-end's chrome for the `ghost-iced/` port. Every value is quoted
from `file:line`. `w`,`h` = logical-px window size. Chrome is raw rects + text (no widgets):
`render()` `main.rs:119-589`, in this order — panes → pane frames+labels → floats(+frames) →
**tab bar → tab context menu → session manager → search overlay → rename overlay → command palette →
status bar → sync chip → wave → toast** (last drawn = topmost, i.e. chrome paints over pane content).

## 0. Constants, fonts, coordinate conventions

| thing | value | source |
|---|---|---|
| `TAB_BAR_H` / `STATUS_BAR_H` | `30.0` / `26.0` | `app.rs:26-27` |
| status-bar height in render | local `let sh = 26.0_f32;` — **duplicate literal**, not the constant | `main.rs:444` |
| `content_rect` | `{x:0, y:TAB_BAR_H, w:w, h:max(h - TAB_BAR_H - STATUS_BAR_H, 1.0)}` | `app.rs:2072-2081` |
| window | 1280×800 default, resizable, `high_dpi: true`; drawing is logical px (`let dpi = 1.0`) | `main.rs:619-622`, `app.rs:201` |
| `padding` / `row_gap` | `config.padding` default `6.0` / `config.row_gap` default `4.0` | `config.rs:91-92` |
| terminal cell font px | `round(font_size * 96/72).max(6.0)` → 12 pt default = **16 px** | `app.rs:202` |
| chrome font | `app.font` = `assets/JetBrainsMono-Medium.ttf` (= iced `fonts::PRIMARY_BYTES`) | `main.rs:661`, `ghost-iced/src/main.rs:33` |

**Chrome font sizes are hardcoded px and do NOT scale with `config.font_size`:** 16 palette input;
15 palette rows; 14 tab label, search box, rename box; 13 status bar/hints/theme name/sync chip/toast/
session rows/menu items/pane labels; 12 session hint line; 11 ssh marker.

**Baseline vs top-left** — macroquad text y = **baseline**, iced = top-left. Centering always uses
`measure_text(label, size, 1.0)`. Observed baselines: tab label 14 px → `TAB_BAR_H/2 + 4` (=19 in a
30 px bar, centre +4); status bar 13 px → `h - 8`; toast 13 px → `h-sh-12` in a 24 px box (18 from box
top); session row / menu item 13 px → `box_top + item_h/2 + 5`; palette row 15 px → `by+48+i*28` with
the highlight top at `y-18`; search/rename 14 px → `TAB_BAR_H+26` in a 30 px box. Rule of thumb
**baseline ≈ box centre + 5 px at 13 px, + 4 px at 14 px** — per-element sizes differ, follow the list
rather than normalising. Box widths are always `measure_text(...).width + pad` ⇒ in iced measure the
text and add the same pad; fixed widths will not match.

## 1. TAB BAR (top, height 30)

- Bar background: `draw_rectangle(0, 0, w, TAB_BAR_H)` in **`theme.tab_bg`** (`main.rs:267`).
- Tab width: local literal `let tab_w = 130.0_f32;` (`main.rs:268`), `x = i * tab_w` unclamped — no
  wrap/scroll/ellipsis, so overflow tabs are simply off-screen (`main.rs:270`).
- Per tab (`main.rs:269-285`), `active = i == app.active_tab`:
  - chip rect `(x, 0, tab_w - 4.0, TAB_BAR_H)` = **126×30**, fill **`theme.tab_active`** when active else
    **`theme.tab_bg`** (`main.rs:272`). Inactive chips are therefore *invisible*; the untouched 4 px
    right gap of `theme.tab_bg` reads as a separator. **No underline/indicator, no border** — the
    highlight is the filled chip alone.
  - label: `tab.name` if non-empty, else `format!("Tab {}", i + 1)` (`main.rs:273-277`); text colour
    **`theme.fg`** when active else **`theme.tab_inactive_text`** (`main.rs:278`); drawn at `x + 10.0`,
    baseline `TAB_BAR_H / 2.0 + 4.0`, **font 14** (`main.rs:279-284`).
  - `Tab::name` is overwritten by the active pane's OSC title whenever it is empty **or starts with
    `"Tab "`** (`app.rs:2961-2969`). First tab: `"Tab 1"` (`app.rs:249`); `new_tab()`:
    `format!("Tab {}", tabs.len() + 1)` (`app.rs:756`) — so a new tab can collide with an existing
    `"Tab N"` name, and both get replaced by the shell title on the next frame.
- **Right-aligned items: none** — the sync chip, theme name and wave live in the status bar (§2).
- **No `+` / new-tab affordance and no hit region for one** exists anywhere in the draw path; new tab =
  `Ctrl+Shift+T` (`keymap.rs:242`), tab-mode `n` (`keymap.rs:422`) or palette `New Tab`, and the
  right-click menu does not offer it either.
- Hit test (`app.rs:2591-2607`): `my < TAB_BAR_H` → left press
  `idx = (mx / tab_w) as usize; if idx < tabs.len() { goto_tab(idx) }`; right press → same index,
  `tab_menu = Some((idx, mx, my))`. **Quirk/bug: the hit test uses `tab_w = 120.0` while the renderer
  uses 130.0** (`app.rs:2593,2601` vs `main.rs:268`) — the right 10 px of each chip selects the next
  tab. Keep 130 in the port.

## 2. STATUS BAR (bottom, height 26) — `main.rs:443-573`

1. Background `(0, h - sh, w, sh)` in **`theme.status_bg`** (`main.rs:445`).
2. Mode label box: `mode_label = format!(" {} ", app.mode.label())` — **spaces both sides**
   (`main.rs:446`); `mw = measure_text(mode_label, 13).width + 12.0` (`main.rs:447`);
   box `(0, h - sh, mw, sh)` in **`theme.frame_active`** (`main.rs:448`);
   text at `x = 6.0`, baseline `h - 8.0`, **font 13**, colour **`theme.bg`** (`main.rs:449-454`).
   `Mode::label()` literals (`keymap.rs:26-42`): `NORMAL PANE TAB RESIZE SCROLL SEARCH SEARCH> MOVE
   SESSION LOCKED "RENAME TAB" "RENAME PANE" "RENAME SESSION"`.
3. Hints: `app.mode.hints()` at `x = mw + 10.0`, baseline `h - 8.0`, **font 13**,
   colour **`theme.status_fg`** (`main.rs:455-461`). Strings (`keymap.rs:45-61`):
   - NORMAL `Ctrl+p pane | Ctrl+t tab | Ctrl+n resize | Ctrl+s scroll | Ctrl+h move | Ctrl+o session | Ctrl+g lock | Ctrl+q quit`
   - PANE `n new | d down | r right | s stacked | x close | h/j/k/l focus | p cycle | f float | e hide | E cycle float | z frames | c rename | Esc exit`
   - TAB `n new | x close | h/l prev/next | r rename | b break | 1-9 goto | Tab last | Esc exit`; RESIZE `h/j/k/l resize | H/J/K/L shrink | =/+ grow | - shrink | Esc exit`
   - SCROLL `j/k scroll | d/u half page | s search | Ctrl+c bottom | e edit | Esc exit`; SEARCH `n/p next/prev | c case | w wrap | o whole word | Ctrl+s done | Esc exit`
   - ENTERSEARCH `type to search | Enter done | Esc cancel`; MOVE `h/j/k/l move pane | n next | p previous | Esc exit`; SESSION `w session manager | d detach (save+quit) | Esc exit`
   - LOCKED `Ctrl+g unlock (input passes through)`; RENAME\* `type new <tab|pane|session> name | Enter confirm | Esc cancel`
4. Theme name (right-aligned): `right = format!(" {} ", app.theme_name)`, `rw = measure + 12`;
   at `x = w - rw`, baseline `h - 8.0`, **font 13**, **`theme.status_fg`** (`main.rs:462-469`).
   `theme_name` is `config.theme` verbatim (`app.rs:193`) even when `theme::by_name` fell back to
   `tokyonight` for an unknown name (`theme.rs:100-106`) — display quirk.
5. Sync chip, only when `app.current_tab().sync_panes`: label `" [SYNC] "`,
   `sw = measure(label, 13).width + 8.0`, `sx = w - rw - sw - 4.0`,
   rect `(sx, h - sh + 2.0, sw, sh - 4.0)` (2 px vertical inset) in **`theme.search_match`**, text at
   `sx + 4.0`, baseline `h - 8.0`, **font 13**, colour **`theme.bg`**; no border (`main.rs:471-482`).
   Toggle = `Action::ToggleSyncPanes` (palette, pane-mode `b`, tab-mode `s`), which also toasts
   `synchronized input ON (broadcasting to all panes)` / `synchronized input OFF` (`app.rs:626-638`).

### Wave ("ghostwave" visualizer) — `main.rs:484-573`, state `app.rs:182-183,280-281,2904-2920`

Gated on `config.visual_wave` (default `true`, `config.rs:95`).

| parameter | value |
|---|---|
| `hints_w` | `measure_text(hints, 13).width` (the §2.3 string) |
| `wave_start_x` | `mw + 10.0 + hints_w + 24.0` |
| `wave_end_x` | `w - rw - 24.0`, or `w - rw - sw - 24.0` when the sync chip shows |
| draw guard | only if `wave_end_x > wave_start_x + 50.0` (`main.rs:495`) — no wave in narrow windows |
| `center_y` | `h - sh/2.0` (status-bar vertical centre) |
| `step` | `3.0` px; `num_points = (wave_w/step).max(10.0) as usize`; loops run `0..=num_points` ⇒ **num_points+1 samples** |
| `progress` | `i / num_points` ⇒ the last sample lands exactly on `wave_end_x` |
| `envelope` | `sin(progress * PI)` — 0 at both ends, 1 in the middle |
| `max_amp` | `(sh * 0.35 * config.wave_amplitude).clamp(2.0, 10.0)` = **9.1 px** at defaults |

Two harmonics, polylines between consecutive samples, alpha-blended (not premultiplied):

- **H1** (primary ribbon): `freq = 0.035`,
  `y_off = (sin(x*freq + phase)*0.7 + sin(x*freq*1.8 - phase*1.4)*0.3) * max_amp * energy * envelope`,
  `y = center_y + y_off`; stroke **1.5**; colour **`theme.frame_active`**;
  `alpha = (0.20 + 0.50*min(energy/2.0, 1.0)) * envelope` (`main.rs:507-538`).
- **H2** (counter-phase glow): `freq = 0.022`,
  `y_off = (sin(x*freq*1.3 - phase*0.8)*0.8 + sin(x*freq*2.2 + phase*1.9)*0.2) * (max_amp*0.65) * energy * envelope`;
  stroke **1.0**; colour **`theme.palette_sel`**;
  `alpha = (0.12 + 0.30*min(energy/2.0, 1.0)) * envelope` (`main.rs:540-571`).
- H1 is drawn fully before H2 (H2 on top). Peak offset `max_amp*energy = 22.75 px` vs `sh/2 = 13` ⇒ the
  ribbon deliberately overflows the bar; no clip, no wave-coloured background.

Energy / timing:

- init `wave_energy = 0.1`, `wave_phase = 0.0` (`app.rs:280-281`).
- **key press** (`App::handle_key`, any key, only when `visual_wave`):
  `wave_energy = (energy + 0.25).min(2.5)` (`app.rs:2153-2156`).
- PTY output in flight: `+0.12` per frame, same clamp (`app.rs:2906-2908`).
- decay: `energy = (energy * exp(-3.713 * dt)).max(0.1)` (`app.rs:2909-2910`) with
  `dt = get_frame_time().clamp(0.001, 0.1)` (`app.rs:2902`) ⇒ half-life ≈ **0.187 s**, floor **0.1**
  (the wave never stops).
- phase: `speed_mult = if energy > 0.15 { 1.0 + energy*1.5 } else { 0.35 }`;
  `phase += 2.4 * dt * config.wave_speed * speed_mult`; wrapped by `-= TAU*100`
  (`app.rs:2911-2919`). Idle 0.84 rad/s; at max energy 3.77 rad/s.
- This is a per-frame animation: main.rs paces 120 FPS while `pty_activity`, 60 FPS while "active"
  (input/mouse/`toast.is_some()`/`wave_energy > 0.15`/visual pane — `main.rs:692-697`), else 8 FPS
  (`main.rs:723-733`). In iced it needs an explicit ~16 ms tick subscription while
  `config.visual_wave && wave_energy > 0.15` (iced redraws on state change only).

## 3. TOASTS — draw `main.rs:575-588`; state `app.rs:147,711-716,2884-2890`

- `pub toast: Option<(String, Instant)>`; `App::toast(msg)` **replaces** it ⇒ **one at a time, never
  stacked**, no queue, no fade, no icon.
- `label = format!(" {msg} ")`; `lw = measure_text(label, 13).width + 16.0` (`main.rs:578-579`).
- Box `(8.0, h - sh - 30.0, lw, 24.0)` in **`theme.palette_sel`**, **no border** (`main.rs:580`).
- Text `x = 16.0`, baseline `h - sh - 12.0`, **font 13**, colour **`theme.bg`** (`main.rs:581-586`).
- Anchor: **bottom-left**, 8 px from the left edge, bottom edge 6 px above the status bar
  (`h-sh-30 + 24`), grows rightwards.
- Lifetime: **drawn while `t0.elapsed().as_secs_f32() < 2.5`** (`main.rs:577`) but cleared at
  **`>= 3` s** (`app.rs:2886`) ⇒ 0.5 s where it is invisible yet `toast.is_some()` keeps the loop in
  60 FPS mode (`main.rs:696`). **Use 2.5 s in the port; don't copy the mismatch.**
- Appearing and expiring both call `repaint_all()` (the box covers pane content).
- Message format strings for wording parity: `theme: {name}` (`app.rs:1687`), `opened {path}`
  (`app.rs:1307`), `copied`/`nothing to copy` (`app.rs:561`), `session: {name}` (`app.rs:1817`),
  `deleted session: {name}` (`app.rs:2317`), `unlocked` (`app.rs:2162`),
  `pane floating — f to embed, Ctrl+h move, Ctrl+n resize` (`app.rs:1063`).

## 4. COMMAND PALETTE overlay — `main.rs:399-441`

- Open `Ctrl+Shift+P` → `Action::OpenCommandBar` (`keymap.rs:234`); closes on Esc, Enter, or **any
  left click anywhere** (`app.rs:2611-2615`).
- Box: `bw = (w * 0.6).min(640.0)`, `bh = 360.0` fixed, `bx = (w - bw)/2.0`, **`by = 60.0` fixed**
  (not vertically centred) (`main.rs:401-404`); fill **`theme.palette_bg`**, border 1 px
  **`theme.frame_active`** (`main.rs:405-406`).
- Input line `format!("> {}", pal.input)` at `bx + 12.0`, baseline `by + 28.0`, **font 16**,
  **`theme.palette_fg`** (`main.rs:408-414`); no caret glyph. Separator: 1 px line
  `(bx+6, by+38) → (bx+bw-6, by+38)` in **`theme.frame`** (`main.rs:415`).
- Rows (`main.rs:417-432`): `visible = 10.min(filtered.len())`; row `i`:
  `y = by + 48.0 + i as f32 * 28.0` (this `y` is the **baseline**);
  `selected = i == pal.cursor.min(filtered.len().saturating_sub(1))`;
  when selected, highlight `(bx + 4.0, y - 18.0, bw - 8.0, 26.0)` in **`theme.palette_sel`**
  (26 vs 28 pitch ⇒ 2 px gap between rows); label at `bx + 14.0`, **font 15**, colour
  **`theme.bg`** if selected else **`theme.palette_fg`**.
  **Quirk: no scrolling — only rows 0..10 exist and `selected` is only true in that range, so with
  >10 matches and the cursor past index 9 nothing is highlighted (Enter still runs the off-screen
  item).** The port should scroll to keep the cursor visible.
- Empty state `"no matching command (try: open <path> | ssh <host> | theme <name>)"` at `bx + 14.0`,
  baseline `by + 66.0`, **font 14**, **`theme.tab_inactive_text`** (`main.rs:433-440`).
- Filtering (`palette.rs:29-68`; items `palette.rs:103-139`): `strip_prefix("open ")` + non-empty
  remainder → exactly one row `format!("Open file: {p}")` (trimmed); `"ssh "` → `format!("Connect SSH:
  {h}")`; `"ds "` → `format!("Durable SSH (ghostd): {h}")`; a bare prefix falls through. Otherwise
  case-insensitive substring on the label (`input.to_lowercase()`, empty = all).
  `input.starts_with("theme")` keeps only `"Theme:"` labels (themes appended as
  `format!("Theme: {t}")` for each `theme::names()` entry). `type_char`/`backspace` reset `cursor = 0`;
  `up`/`down` wrap modulo the filtered length; `selected()` clamps the cursor to the last item.

## 5. SESSION MANAGER overlay + TAB CONTEXT MENU

### Session manager — `main.rs:308-355`, data `app.rs:2057-2068`, keys `app.rs:2260-2341`

- Opens from session mode `w` (`keymap.rs:518` → `Action::SessionManager`). While open it **swallows
  every key** except those below (`app.rs:2340`); it has **no mouse support at all**.
- Entries: `session::list_sessions()` (sorted non-empty `.json` names, `session.rs:112-115`); if
  `current_session` is absent it is pushed and the list re-`sort()`ed; selected = its index
  (`app.rs:2058-2066`).
- Box: `w = 460.0`, `item_h = 24.0`, `title_h = 30.0`, `hint_h = 22.0`,
  **`h = title_h + entries.len()*item_h + hint_h`** (unbounded, no scroll), `x = (w_win - 460)/2.0`,
  **`y = (h_win - h)/3.0`** (upper third, not centred) (`main.rs:310-316`). Fill **`theme.bg`**,
  border 1 px **`theme.tab_bg`** (`main.rs:317-318`). Title `format!("SESSIONS — current: {}",
  app.current_session)` (em dash U+2014) at `x + 12.0`, baseline `y + 20.0`, **font 13**,
  **`theme.search_match`** (`main.rs:319-324`).
- Row `i`: `yy = y + title_h + i*item_h`; if `i == selected` fill `(x, yy, w, item_h)` in
  **`theme.tab_active`** (no border, text colour unchanged) (`main.rs:326-329`); label
  `format!("● {name}")` when it is the current session else `format!("  {name}")` (U+25CF + space, or
  two spaces) at `x + 12.0`, baseline `yy + item_h/2 + 5.0`, **font 13**, **`theme.palette_fg`**
  (`main.rs:330-337`).
- ssh marker when `session::has_remote(name)` (any pane with `remote.is_some()`, `session.rs:151-154`):
  text `"ssh"` at `x + 16.0 + lw` (`lw = measure_text(&label, 13).width`, i.e. 4 px past the label,
  which starts at `x+12`), same baseline, **font 11**, **`theme.search_match`** (`main.rs:338-347`).
- Hint line: `"↑/↓ select | Enter switch | n new | r rename | d delete | k kill remote | Esc close"`
  at `x + 12.0`, baseline `y + h - 8.0`, **font 12**, **`theme.status_fg`** (`main.rs:349-354`).
- Keys (`app.rs:2262-2339`): Esc close; Up `sel.saturating_sub(1)`; Down `min(sel+1, len-1)`; Enter →
  `load_session(selected)` + mode Normal; `r` → `Mode::RenameSession` with `rename_buf`/`rename_target`
  = selected name (`app.rs:2287-2295`); `d` → delete, refusing the current session and dropping the
  parked copy, then refresh + clamp `sel` and toast `deleted session: {name}` /
  `cannot delete the current session`; `k` → `kill_session_remotes`; `n` → new `session-{i}`
  (`app.rs:2044-2053`).

### Tab context menu — `main.rs:287-306`, constants `app.rs:20-24`, clicks `app.rs:2567-2588`

- Opens on **right press in the tab bar** at the click point (`app.rs:2596`), extends right and down
  over the content; not clamped to the window.
- `TAB_MENU_W = 230.0`, `TAB_MENU_ITEM_H = 24.0`, `TAB_MENU_PAD = 8.0`, `TAB_MENU_ITEMS = 2`;
  `menu_h = PAD + ITEMS*ITEM_H + PAD = 64.0` (`main.rs:290`).
- Rect `(mx, my, 230, 64)` fill **`theme.bg`**, border 1 px **`theme.tab_bg`** (`main.rs:291-292`).
  Items — a literal array duplicated from `TAB_MENU_ITEMS`: `"Rename tab"`, `"Close tab"`
  (`main.rs:293-296`); item `i` at `mx + 12.0`, baseline `my + PAD + i*ITEM_H + ITEM_H/2 + 5.0`
  (= `my+25` / `my+49`), **font 13**, **`theme.palette_fg`** (`main.rs:297-305`). **No hover or
  selected-row state, no keyboard navigation.**
- Click (`app.rs:2568-2587`): on any left press, if inside the box,
  `item = ((my - my0 - PAD) / ITEM_H) as usize` → `0` = goto tab + `Mode::RenameTab`, `1` = goto +
  `close_tab()`; menu closes, `repaint_all()`. **Quirks: the top 8 px of padding casts to item 0
  (renames), the bottom padding casts to 2 (no-op); Rust's `f32 as usize` saturates negatives to 0.**
- Keys while open: Esc closes, everything else swallowed (`app.rs:2220-2227`).

## 6. SEARCH bar overlay — `main.rs:357-381`

- Only in `Mode::EnterSearch` (scroll mode `s` → `SearchStart`, `keymap.rs:469`; or `Ctrl+Shift+F` →
  `OpenSearch`) and only when an active terminal pane exists.
- `total = t.search.matches.len()`, `cur = t.search.current.min(total.saturating_sub(1))`
  (`main.rs:360-361`) ⇒ **quirk: 0 matches renders `1/0`** (label always prints `cur + 1`).
- `q` = query, else placeholder literal `"type to search…"` (U+2026) (`main.rs:362`).
- `flags = format!("{} {} {}", …)` (`main.rs:363-368`): `"[c]ase ✓"` / `"[c]ase"`, `"[w]rap ✓"` /
  `"[w]rap"`, `"[o]word ✓"` / `"[o]word"` (U+2713). Defaults from `SearchState::default()`:
  `case_sensitive = false`, `wrap = true`, `whole_word = false` (`search.rs:26-38`) ⇒ initial render
  `[c]ase [w]rap ✓ [o]word`.
- Exact label: `format!(" 🔍 {}   {}/{}   {flags}", q, cur + 1, total)` — leading space, magnifier
  U+1F50D, **three spaces** either side of the counter (`main.rs:369`).
- Geometry: `lw = measure_text(label, 14).width + 24.0`, `bx = (w - lw)/2.0`,
  box `(bx, TAB_BAR_H + 6.0, lw, 30.0)`, fill **`theme.palette_bg`**, border 1 px
  **`theme.frame_active`**; text `bx + 12.0`, baseline `TAB_BAR_H + 26.0`, **font 14**,
  **`theme.palette_fg`** (`main.rs:370-379`). Width grows with the query; no max, no truncation.
- Per-cell highlight is **not** in main.rs: `term.rs:929-931` draws a matched cell as fg `theme.bg`
  on bg **`theme.search_match`**; a selected cell swaps fg/bg (`term.rs:924-927`) and an empty selected
  cell uses **`theme.selection`** (`term.rs:883-888`). Search is viewport-scoped only
  (`search.rs:1-2`) and recomputed when `search.dirty`. Reuse `TermPane`'s render path in the port —
  don't re-derive cell colours.
- Keys in this mode (`keymap.rs:332-346`): Esc / Ctrl+C cancel, Enter → mode `Search`, Backspace edits
  the query, any other text char appended.

## 7. RENAME prompt + other small overlay text

- Drawn **only for `Mode::RenameTab` and `Mode::RenamePane`** (`main.rs:384`). **`Mode::RenameSession`
  has no input box** — the manager's rename only shows `RENAME SESSION` in the mode label and its hint
  (`keymap.rs:40,59`). Fix in the port.
- `what = "Tab"|"Pane"`; `label = format!(" rename {what}: {}_", app.rename_buf)` — leading space plus
  a literal trailing `_` caret (no cursor position, no selection, no width cap) (`main.rs:385-386`).
- Geometry identical to §6: `lw = measure(text, 14).width + 24.0`, `bx = (w-lw)/2.0`, box
  `(bx, TAB_BAR_H + 6.0, lw, 30.0)`, fill **`theme.palette_bg`**, border 1 px **`theme.frame_active`**;
  text `bx + 12.0`, baseline `TAB_BAR_H + 26.0`, **font 14**, **`theme.palette_fg`**
  (`main.rs:387-396`).
- `rename_buf` is pre-filled with the current name when started from the manager (`app.rs:2293`);
  Enter confirms (empty = no change, `app.rs:668-684`), Esc cancels, Backspace is
  `Action::Backspace` (`keymap.rs:322-323`).
- No other chrome text exists. Pane-local overlays (preview/AI/visualizer, `preview.rs`, `ai.rs`,
  `visualizer.rs`) are pane content, and pane frame labels (chip `(rect.x-2, rect.y-14, lw, 14)` at
  font 13, `theme.tab_bg` on the frame colour) are M1, already ported.

## 8. Theme-field usage map (`theme.rs:17-49`; 8 themes `theme.rs:89-98`)

Fields (`theme.rs:17-49`; 8 themes `theme.rs:89-98`): `bg, fg, cursor, tab_bg, tab_active,
tab_inactive_text, frame, frame_active, status_bg, status_fg, palette_bg, palette_fg, palette_sel,
selection, search_match`.
Tab bar → `tab_bg`/`tab_active`/`tab_inactive_text`/`fg`. Status bar → `status_bg`, `status_fg`,
`frame_active` (mode box), `bg` (mode-box, sync-chip and toast text), `search_match` (sync-chip fill).
Overlays → `palette_bg`/`palette_fg`/`palette_sel`, `frame` (palette separator), `frame_active`
(palette/search/rename borders), `tab_bg` (session box + menu borders), `tab_active` (session selected
row), `search_match` (session title, ssh marker), `tab_inactive_text` (palette empty state).
Wave → `frame_active` (H1), `palette_sel` (H2). Toast → `palette_sel` fill + `bg` text.

## 9. Do NOT copy verbatim (bugs / dead code / hardcoded values)

1. Tab hit-test `120.0` vs render `130.0` (`app.rs:2593,2601` vs `main.rs:268`).
2. Toast drawn < 2.5 s but cleared at 3 s (`main.rs:577` vs `app.rs:2886`).
3. Palette has no scrolling, so the selection can be invisible with >10 matches (`main.rs:418-422`).
4. Search counter shows `1/0` with no matches (`main.rs:361,369`).
5. Rename overlay missing for `Mode::RenameSession` (`main.rs:384`).
6. Status-bar height duplicated as `26.0` instead of `STATUS_BAR_H` (`main.rs:444`).
7. Chips are 126 px wide with a 4 px gap, unbounded count, labels unclipped (`main.rs:268-284`).
8. Session box grows unbounded; `y = (h - box_h)/3` goes negative for long lists (`main.rs:314-316`).
9. `theme_name` displays the configured name even after a silent theme fallback
   (`app.rs:193`, `theme.rs:100-106`).
10. Frame labels: `format!("{n} {i} — {title}")` is 0-based while the no-title branch uses `i + 1`, and
    float labels use `i` for every kind (`main.rs:176-183`, `main.rs:244-251`). Also the top-row label
    chip (`rect.y - 14`) is **overdrawn by the tab bar**, painted later (`main.rs:164-196` before
    `main.rs:266`) — the same layering in iced will hide it again.
11. Palette/session/menu close on *any* left click (`app.rs:2611-2615`); the session manager ignores
    the mouse entirely (`app.rs:2567+`).
12. Palette is fixed 360 px tall at `y = 60` regardless of window height; wave and hints vanish below
    ~`mw + hints_w + 74` window width (`main.rs:401-404`, `main.rs:495`).
