# Retirement plan: archive the macroquad front-end

Written 2026-09-17 against `master` `8c40aaf` / `feat/iced-frontend` `e492c2c`.

## 1. Decision and scope

The iced front-end is the future of this project: it renders better, and panes, overlays and the
rest of the UI are toolkit built-ins instead of hand-rolled game-engine work. The macroquad
front-end will therefore be **archived to its own branch and removed from the ongoing branches**,
not developed further and not run in parallel.

This document is the plan for that cut: what must be true first, what goes where, in what order, and
how to undo it. Nothing here is done yet.

## 2. Preconditions (the parity gate)

Do not cut until the iced front-end does everything that is actually used day to day. The
macroquad build is the only implementation of:

- selection, copy/paste and clipboard integration (including middle-click paste)
- scrollback search (overlay + jump/regex modes)
- the command palette
- durable sessions and the SSH/`ghostd` path (session save/restore, pin/rename semantics)
- floating panes and hidden/background panes (workspaces, `Ctrl+Tab`), including the F12 / `Ctrl+~`
  scratchpad (a floating terminal — float parity, not a content pane)
- the preview pane, the AI pane and the visualizer pane — **all three are wanted** (see below)
- kitty graphics (inline images)
- config extras that only the macroquad front-end reads today (cursor style, and whatever
  `src/config.rs` grew that the port never picked up)

Reconcile this list against `README.md` and `keymap::Action` before cutting — the enum is the
authoritative list of what the app can do, and the README is the public claim. Then get the owner
to walk the resulting checklist in a real window; "the test suite is green" is not the gate.

**Owner's call on the three content panes** (2026-09-17): all three are wanted, the visualizer
especially. Note that they have never been exercised by hand — the F12 pane that was tested is the
*scratchpad*, a floating terminal, not these. Cost and feasibility differ per pane:

- **visualizer**: no macroquad-specific capability is involved — it draws only lines, rectangles and
  text (five `draw_line`, three `draw_rectangle`, one `draw_rectangle_lines`, three `draw_text_ex`,
  no textures, no camera, no render target, no clock of its own), so the "3D" is projection math
  over 2D primitives. Porting it onto an iced canvas widget keeps the math and rewrites the draw
  calls.
- **preview**: the image branch needs a decoded texture; iced has image widgets and canvases, so it
  is a rewrite of the viewer, plus the text/dir/binary kinds as ordinary widgets.
- **AI**: mostly text and a worker thread — the smallest port; it becomes a widget tree plus the
  existing blocking-HTTP-in-a-thread pattern.

How each is reached today, since the iced build has to reproduce the entry points as well as the
panes: the visualizer and the AI pane open from the command palette (`Ctrl+Shift+P` → "Ghostwave
(Screensaver Visualizer)" and "New AI Chat"); the preview pane has no palette entry and opens from
`Ctrl+Click` on a file path; the scratchpad is `F12` / `Ctrl+~`. The AI pane reads `[ai]` from
`~/.config/ghostwave/config.toml` (`base_url` / `model` / `api_key_env` / `system_prompt`, any
OpenAI-compatible endpoint) and takes a copy of it at pane creation, so config edits need a restart
plus a new pane.

If porting the visualizer proves too costly at cut time, the standing fallback is to keep a minimal
macroquad binary for that pane alone. Its costs should be weighed then, not assumed: macroquad stays
in the dependency graph, that binary needs the terminal core, and the core would either stay shared
by both crates or split into its own crate. Default expectation is porting; decide at the cut.

## 3. The archive branch

- **Name**: `macroquad-archive`. Cut it from the **last commit on which the macroquad build
  compiles, runs and passes its tests** — not from wherever the tree happens to be when someone
  remembers. Record that SHA in the archive branch's README.
- **Tag** the same commit (`macroquad-final`) and push both. Branches get deleted and renamed over
  years; a tag is the stable pointer, and the public mirror keeps the code reviewable.
- **Make the branch self-sufficient**: add a short README at its root covering what this build is,
  why it was retired, how to build and run it at that commit (`nix-shell ~/.hermes/shell.nix --run
  "cargo run"`), and the gotcha that replicas need the project shell's `LD_LIBRARY_PATH` or
  miniquad panics.
- Do **not** rewrite history to remove the code from `master`'s past. The archive branch plus the
  tag is the record; a rewritten public history is not worth the risk.

## 4. Inventory: what is core, what is macroquad

`src/` is 10,596 lines and is **not** cleanly split — the front-end and the terminal core share a
crate. Classification below is by macroquad reference count and by what `ghost-iced` actually
imports (`use ghostwave::…` in `ghost-iced/src/*.rs`).

| file | lines | macroquad hits | class | action |
| --- | --- | --- | --- | --- |
| `app.rs` | 3253 | 14 | presentation | archive + delete (**but see relocation below**) |
| `term.rs` | 1800 | 9 | core, macroquad types | keep; replace macroquad types |
| `keymap.rs` | 802 | 7 | core, macroquad types | keep; replace macroquad types |
| `main.rs` | 742 | 18 | presentation (entry) | archive + delete |
| `layout.rs` | 731 | 0 | core | keep as-is |
| `session.rs` | 501 | 0 | core | keep as-is |
| `fonts.rs` | 370 | 6 | core, macroquad types | keep; folds into the port's font stack |
| `pty.rs` | 313 | 0 | core | keep as-is |
| `palette.rs` | 308 | 0 | presentation (overlay) | archive + delete |
| `ai.rs` | 293 | 1 | presentation (pane) | archive + delete (or port — see §2) |
| `search.rs` | 289 | 0 | core logic | keep (the iced search UI is new work) |
| `preview.rs` | 282 | 1 | presentation (pane) | archive + delete (or port) |
| `visualizer.rs` | 271 | 1 | presentation (pane) | archive + delete (or port) |
| `config.rs` | 243 | 0 | core | keep as-is (shared config lives here) |
| `clipboard.rs` | 223 | 0 | core | keep as-is |
| `theme.rs` | 156 | 0 | core | keep as-is |
| `lib.rs` | 19 | 0 | core (module list) | keep; drop the deleted modules |

**The one hard blocker:** `ghost-iced` imports `app::{STATUS_BAR_H, TAB_BAR_H}` — two constants
that live in the file being deleted. Relocate them to a core module (e.g. `theme.rs` or a small
`geometry`/`layout` home) *before* deleting `app.rs`, and update the port's import.

**De-macroquad passes** are the second piece of real work: `term.rs`, `keymap.rs` and `fonts.rs`
carry macroquad types (colours, key codes, the font handle type). Each needs its macroquad-typed
surface replaced with the core's own type or the iced equivalent. The port already has its own
`keys.rs` (iced → `libghostty` `KeyPress`) and font stack, so part of this is deleting duplicated
tables rather than writing new ones — decide per type, do not blanket-replace.

## 5. The cut, in order

Do it on a short-lived branch (`feat/retire-macroquad`), because `master` is protected.

1. **Freeze the gate**: the §2 checklist walked by hand in the iced build, tests green, tree clean.
2. **Archive**: create `macroquad-archive` + tag `macroquad-final` from the last known-good
   macroquad commit, add its README, push both to forgejo and the public mirror. Verify by cloning
   the branch fresh and building it.
3. **Relocate** `STATUS_BAR_H` / `TAB_BAR_H` out of `app.rs`; point the port's import at the new
   home; tests green.
4. **De-macroquad the core** (`term.rs`, `keymap.rs`, `fonts.rs`) against the port's needs only;
   tests green at each step.
5. **Move the iced front-end in**: `ghost-iced/src/*` becomes the root crate's sources, the
   `[workspace]` marker goes away, the second `Cargo.toml`/`Cargo.lock` collapse into one, the
   binary name becomes `ghostwave`, and `macroquad` (plus whatever of `image`/`png`/`ureq`/
   `arboard`/`fontdue` only the presentation used) leaves the dependency list. Keep `nix`,
   `libghostty-vt`, `serde`/`toml`, `fontdue` if the font fallback still needs it.
6. **Delete** the presentation files listed in §4, and the modules from `lib.rs`.
7. **Docs**: rewrite `README.md` so every claim is a feature the iced build actually has (this repo
   has been burned by unexercised README claims before); add a `PROGRESS.md` section that keeps the
   macroquad history and points at the tag; update `PORT-PLAN.md`'s framing (it still treats the
   macroquad build as the acceptance reference).
8. **Verify**: `cargo test` on the single crate, `cargo run` in a real window, walk the §2 checklist
   again on the merged result, `git log --oneline --stat` to confirm only intended paths changed.
9. **Propagate**: merge `master` into any live dev branch, or delete the stale ones
   (`perf/idle-gpu-2`, `selection-autoscroll`, `feat/visual-ghostwave` — check each for unmerged
   work first). Push `master` and the mirror.

## 6. Rollback

Nothing is lost by cutting: the code stays reachable on `macroquad-archive` and at the tag, and
history is not rewritten. If the move goes wrong, revert the retire commit (or reset the branch to
the pre-cut SHA) and fix forward on the archive branch's basis. The only irreversible mistake would
be forcing the archive branch/tag away before it is pushed.

## 7. Traps

- Deleting `src/**` wholesale — the core lives there too (§4).
- Cutting on "the tests pass" without the owner walking the feature checklist: the missing pieces
  (selection, search, palette, sessions, floats) are exactly the ones no unit test covers.
- Leaving the port's crate structure in place: after the cut the root crate must *be* the iced app,
  or the default binary is the thing that was archived.
- Publishing a README that describes the old build, or claims features that are not exercised.
- Forgetting `forgejo/master` when pushing: the protected remote is the one that matters.

## 8. Open questions for the owner

1. Which shape at step 5: fold the iced sources into the root crate (keeping the package name and
   the `path = ".."` dependency only where it still makes sense), or promote the iced crate to the
   root package name? The first keeps one crate and one `Cargo.toml`; the second keeps the port's
   crate boundary and needs the core split into its own crate.
2. Preview / AI / visualizer panes: all three are wanted (see §2), so the question is only *how*
   they land. Default expectation is porting onto iced (canvas/widgets); the fallback of keeping a
   minimal macroquad binary for the visualizer alone is available and deferred to cut time.
3. Does the archive branch need anything beyond a README — e.g. a released binary, or screenshots —
   for the public repo to stay honest about what the retired build did?
