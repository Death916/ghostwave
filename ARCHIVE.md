# This branch is an archive

This is the **macroquad front-end** of ghostwave, retired on 2026-09-17. It is not developed
further and not merged into `master` again.

The front-end that replaced it is the iced 0.14 port, which lives on `master` (and grew on
`feat/iced-frontend`). It renders better and gets panes, overlays, focus and the rest of the UI from
the toolkit instead of hand-rolling them on a game engine.

## Where this code lives

- Branch: `macroquad-archive` (this branch)
- Tag: `macroquad-final` — the stable pointer, at `8c40aaf`
- That commit is the last one on which this build was known good: `cargo test` passes there
  (66 + 66 + 1 + 6 tests), and the binary runs in a real window.

History was **not** rewritten to remove this code from `master`'s past; the branch and the tag are
the record, so this stays reviewable.

## Building and running it

```
cd <clone>
nix-shell ~/.hermes/shell.nix --run "cargo run"
```

The interactive shell prints a banner and takes a moment to warm up; the shell prompt matters, not
the banner.

## The one gotcha

Launching the binary **outside** that nix shell panics inside miniquad: the front-end loads the
nix-store shared libraries through the project shell's `LD_LIBRARY_PATH`. Run it from the shell, or
export the same `LD_LIBRARY_PATH` first. The iced port does not have this problem — its binary
carries an rpath.

## What this build still has that the iced port does not

Kept here for reference, because it is the only implementation of these:

- the remote/SSH pane path (`ghostd` sessions: ensure-binary, attach command, connect/kill,
  live-remote tracking)

Everything else — selection and clipboard, scrollback search, the command palette, durable
sessions, floating panes and the F12 scratchpad, the preview/AI/visualizer content panes — was
ported to iced before this branch was cut. See `PROGRESS.md` on `master`.
