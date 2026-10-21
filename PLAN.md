# PLAN — durable sessions as their own type, and a readable durable marker in the session list

Card: t_a4284019 · branch `wt/t_a4284019` · worktree `/home/death916/code/ghostwave/.worktrees/t_a4284019`
Repo: ghostwave (iced front-end + core lib). Base commit: `1a08166`.

## 0. The owner's report, verbatim

> i started a session with ctrl shift p then ssh orac. this was not durable as when i rejoined it it was a new
> shell. we need: 1 durable sessions to be there own type and command and if they are already it should show in
> the command pane when typing durable. and 2 it should show in the session list if its durable or not by a
> label or highlight or something else

### 0.1 Owner follow-up (2026-10-21, after the first plan revision)

> sorry the ds command does work jsut needs to be more obvious

This settles the one thing the plan had left open and narrows the job to discoverability:

- `ds <host>` already produces a durable session — its behaviour is **not** changed by this plan (§3 D3 keeps the
  `ds ` item byte-identical; §4.4 requires the existing `dynamic_ds_prefix` test to pass unmodified).
- The failure was that nothing in the palette or the session list made the difference *visible*: `ssh orac` and
  `ds orac` looked the same, and a durable session was badged `ssh`. So the work is labels + the durable
  inventory, which is what §3 D3/D5/D6 do.
- Consequence for §7: the owner has now said the two commands stay; this plan must not re-litigate that. §7 is
  closed as "keep both, make durable obvious".

## 1. Root cause (verified by reading the code, not assumed)

Two independent defects, one per numbered want.

**(a) The palette cannot show that a durable session already exists, and cannot show that `ssh` is not durable.**
`ssh <host>` (`src/app.rs:1055` `connect_ssh`) spawns a plain `ssh` pty with `remote: None`; the remote shell
dies with the connection. Durable is a *different* command, `ds <host>` (`src/app.rs:1087`
`connect_durable_ssh`), which runs `ssh -t <host> … ghostd attach ghostwave-<host>` and binds the pane to a
`RemoteSpec`. The palette (`src/palette.rs:86-121`) knows both prefixes, but:

- `ds <host>` only ever builds **one** item for the literal text typed — the already-known durable sessions are
  never listed, so there is nothing to "show when typing durable";
- the plain item is titled `Connect SSH: <host>` in category `Remote`, exactly like the durable one, so nothing
  in the palette says which one survives a reconnect (this is how orac was opened non-durably);
- `PaletteState` is built from a static inventory at `src/input.rs:459` (`default_items(&theme::names())`) — it
  has no channel for runtime data at all.

**(b) The session list's marker means the opposite of what it says, and plain ssh has no marker.**
`session::has_remote` (`src/session.rs:151`) is true only for panes with `remote.is_some()` — i.e. durable. The
picker renders that as a lowercase **`ssh`** badge (`src/sessions_overlay.rs:198-212`). So a durable session is
labelled `ssh`, and a plain `ssh <host>` session — the thing that bit the owner — has **no marker at all** and is
indistinguishable from a local session in `Session Manager`.

## 2. Scope — and what is deliberately NOT done

In scope: `src/session.rs`, `src/sessions_overlay.rs`, `src/ui_session.rs`, `src/palette.rs`, `src/input.rs`,
`src/app.rs` (one tab-title string). No new files in `src/`.

NOT done (each is a real gap, called out so review does not read silence as coverage):

- no ghostd change, no daemon protocol change;
- no change to what any existing command *does*: `ds <host>` and `ssh <host>` keep their exact behaviour and
  their existing palette titles (owner, §0.1) — this card is labels + inventory only;
- no new on-disk file/format: no "known hosts" history (see D1 for why);
- no auto-connect on restore — `remote.rs`'s invariant ("a remote pane is never auto-connected on restore") holds;
- no change to `PaneState` / `RemoteSpec` / session JSON: files stay readable by older builds;
- no status-bar / tab-bar badge work beyond D6's tab title; no hover tooltips, no new modal;
- the README "Not in this build" section is already stale (remote/SSH is ported) — left alone, not this card.

## 3. Binding design decisions

**D1 — the durable inventory is read from disk + live panes, not from a new file.**
Sources: new `ghostwave::session::all_remote_specs() -> Vec<(String, String)>` (every saved session's
`(host, daemon session)` pairs, in session-name order) merged with the current workspace's live
`AppState::live_remote_specs(&tabs)` (`src/app.rs:1249`), deduped by host with the existing
`remote::dedupe_hosts` (`src/remote.rs:223`), sorted by host. Rationale: the picker already derives its badges
from the session files ("the current session is written every two seconds", `src/ui_session.rs:47`), and a
durable session that has been killed or deleted no longer exists — a persisted host list would offer targets
that are gone. Cost: one small JSON read per saved session, once per palette open, on the input path.

**D2 — the palette is handed the durable targets, it does not read the disk itself.**
Same rule `SessionsOverlay` already follows (`src/sessions_overlay.rs:1-8`): a widget that reads the data dir
cannot be tested without touching it. `PaletteState` gains `durables: Vec<(String, String)>` (host, daemon
session) and a constructor `PaletteState::with_durables(items, durables)`; `PaletteState::new(items)` keeps its
signature and means "no durable targets", so every existing palette test compiles unchanged.

**D3 — query grammar in `filtered_items` (trimmed, case-insensitive on the prefix).**
Let `q = self.query.trim()`, `lower = q.to_lowercase()`.

| query | items returned |
|---|---|
| `ds <host>` (`lower.starts_with("ds ")`, non-empty host) | **unchanged**: one `Durable SSH (ghostd): <host>`, category `Remote — durable`, `ConnectDurableSsh(host)` |
| `durable` bare (`lower == "durable"`) | one item per known target, **no create item** (a create needs a host): title `Durable session: <host> — <daemon session>`, category `Remote — durable`, `ConnectDurableSsh(host)`; if the inventory is **empty**, fall through to normal fuzzy filtering |
| `durable <text>` (`lower.starts_with("durable ")`) | known targets whose host or daemon session contains `<text>` (case-insensitive), in host order; then, when `<text>` is non-empty, one extra `New durable SSH (ghostd): <text>` (`Remote — durable`, `ConnectDurableSsh(<text>)`) |
| `ssh <host>` | `Connect SSH: <host>` with category `Remote — not durable` (title byte-identical, so the existing test holds), **plus** — only when a known target's host equals `<host>` — `Durable session: <host> — <session>` (`Remote — durable`, `ConnectDurableSsh(host)`) |
| anything else | unchanged fuzzy scoring |

The empty-inventory fall-through for bare `durable` is deliberate: with nothing to list, the existing
"No matching commands" hint is the honest answer, and the hint text is updated (D7) to say how to create one.

**D4 — one core predicate for the three states.** In `src/session.rs`:

```rust
/// Whether a saved session is a durable (ghostd-bound) remote session, a plain
/// ssh one, or neither.
pub enum RemoteKind { Durable, PlainSsh }

/// The remote kind of `name`: `Durable` when any pane carries a [`RemoteSpec`],
/// else `PlainSsh` when any pane's program is `ssh` (how `connect_ssh` spawns
/// one), else `None`, and `None` for a session that does not load.
pub fn remote_kind(name: &str) -> Option<RemoteKind>
```

plus `pub fn all_remote_specs() -> Vec<(String, String)>`. A durable pane wins over a plain one in a mixed
session (`Durable` covers `PlainSsh`). Detection of a plain pane is exact, not heuristic: `connect_ssh` spawns
program `"ssh"` with `remote: None`, and the saved `PaneState.program` is that same string.
`has_remote(name)` stays, re-implemented as `remote_kind(name) == Some(RemoteKind::Durable)`, so `app.rs` and
`ui_session.rs` keep working.

**D5 — the picker carries kinds, not a set.** `SessionsOverlay.remotes: HashSet<String>` becomes
`kinds: HashMap<String, RemoteKind>`; `SessionsOverlay::new(current, names, kinds)`; `is_remote` is replaced by
`is_durable(name)` and `kind(name) -> Option<RemoteKind>`; `relist(names, kinds)`. `ui_session::remote_names()`
becomes `remote_kinds() -> HashMap<String, RemoteKind>` and its three call sites (`open_picker`,
`refresh_picker`, the delete branch of `picker_message`) pass the map through.

Badges in `view` (`src/sessions_overlay.rs:198-212`): `Durable` → `ds` in `theme.search_match` (the highlight
colour the current `ssh` badge already uses), `PlainSsh` → `ssh` in `theme.tab_inactive_text` (dim). The header
line becomes `SESSIONS — current: <name> (durable)` / `(ssh)` / `<name>` so the highlighted row is not the only
place the state is readable. The key-hint line gets no change.

**D6 — the tab title names the type.** `connect_durable_ssh` titles its tab `ds <host>` (today `ssh <host>`,
`src/app.rs:1115`) while plain `connect_ssh` keeps `ssh <host>`. The *session* name stays `ssh-<host>`
(`remote::ssh_session`) for both — that is the documented reason a repeated `ds`/`ssh` on the same host focuses
the existing session instead of stacking a duplicate (`src/remote.rs:70-79`), and it must not drift.

**D7 — the palette's empty hint** becomes
`No matching commands (try: open <path> | ssh <host> | ds <host> | durable)`.

## 4. Ordered work items (stage 3 executes these in this order)

1. `src/session.rs` — add `RemoteKind`, `remote_kind`, `all_remote_specs`, `has_remote` on top of `remote_kind`.
   `all_remote_specs` is a thin fan-out over the **existing** `session::remote_specs(name)` (`src/session.rs:157`,
   today per-session, used by the manager's kill path) over `list()` — reuse it rather than re-walking the JSON.
   Tests (in the existing `mod tests`, which runs against the real data dir with unique names + cleanup —
   see §6 Risks): `a_durable_pane_makes_the_session_durable`, `a_plain_ssh_pane_is_ssh_and_not_durable`,
   `neither_pane_kind_is_neither` (`remote_kind` → `None`), `all_remote_specs_lists_a_durable_binding`
   (assert `contains`, never list equality — the real dir may hold the owner's sessions).
2. `src/ui_session.rs` — `remote_kinds()`, and the three call sites. Rename/retarget the test
   `a_remote_pane_comes_back_disconnected_and_its_session_is_badged_ssh` → `…_is_badged_durable`, asserting
   `remote_kinds().get(&name) == Some(&RemoteKind::Durable)`.
3. `src/sessions_overlay.rs` — `kinds: HashMap`, `is_durable`, `kind`, badges, header, and update the three
   tests that touch `remotes` (`the_current_session_is_listed_and_selected`,
   `a_session_with_a_durable_remote_pane_is_badged_ssh` → `…_is_badged_ds`, `a_relist_keeps_the_selection_on_the_current_session`).
   Add `a_plain_ssh_session_is_badged_ssh_and_a_durable_one_ds` and `a_mixed_session_with_a_durable_pane_is_durable`.
4. `src/palette.rs` — `durables` field, `with_durables`, the D3 grammar, the D3 category strings, the D7 hint.
   Keep `dynamic_ssh_prefix` and `dynamic_ds_prefix` passing **unmodified** (byte-identical titles/actions).
   New tests: `typing_durable_lists_the_known_durable_sessions`,
   `durable_with_a_host_offers_the_new_session_below_the_known_ones`,
   `ssh_beside_a_known_durable_target_offers_the_durable_session`,
   `a_bare_durable_with_an_empty_inventory_lists_nothing_durable`.
5. `src/input.rs` — `Action::OpenCommandBar` builds the targets
   (`remote::dedupe_hosts(live + session::all_remote_specs())`, sorted by host) and calls
   `PaletteState::with_durables(...)`. This is the *action* path, never the render path: `live_remote_specs`
   takes pane mutexes (`src/app.rs:1249-1259`) and `view` must not block on them.
   Update `the_durable_ssh_action_opens_its_own_session_and_binds_the_pane` (`src/input.rs:2199`) to expect tab
   name `ds <host>`; in `the_plain_ssh_action_opens_its_session_without_a_ghostd_binding` add
   `assert_eq!(s.current().name, format!("ssh {TEST_HOST}"))`. Add
   `the_palette_lists_the_durable_sessions_it_was_given` (open, type `durable`, expect the item + action).
6. `src/app.rs` — the D6 tab title (`src/app.rs:1115` only). No app.rs test asserts that string today
   (`src/app.rs:1578` is a test *helper* that names a restored tab `ssh {host}`; leave it, it is just a name).

## 5. Verification — exact commands, expected results

Gate every compile (mandatory, orac is 4 cores / shared):

```sh
cd /home/death916/code/ghostwave/.worktrees/t_a4284019
export CARGO_TARGET_DIR=/var/build/ghostwave/t_a4284019        # per-card; never a shared target dir
bash ~/.hermes/scripts/build_gate.sh nix-shell ./shell.nix --run 'cargo test --all-targets --no-fail-fast'
```

- exit 0 **and** `build_gate.sh` exit 0. Exit 3 from the gate = lock held → comment + block `transient`, do not
  compile anyway. Exit 4 = low RAM → retry later.
- expected: every `test result: ok`, zero `FAILED`, for the four targets the suite runs: `--lib` (core),
  the bin unittests (`src/main.rs` + modules), `tests/flood_throughput.rs`, `tests/term_core.rs`.
  Baseline count at `1a08166`: 204 `#[test]` in `src/` + 7 in `tests/` (measured by `grep -c '#\[test\]'`);
  after this plan the count is baseline + the new tests in §4 and minus none (no test is deleted — the four
  renamed/retargeted ones are the only edits). Stage 3 must paste the real per-target counts.
- a fresh `CARGO_TARGET_DIR` recompiles libghostty-vt with zig (`shell.nix` pins zig 0.15.2); `/var/build` had
  25 GB free at plan time, the last target dir cost 5.3 GB. Use `nix-shell ./shell.nix` — **not** a minimal
  shell (PROGRESS.md:643 records why).
- **Do NOT run `cargo fmt --check`**: measured on this tree, it already fails at `1a08166` with pre-existing
  diffs in `src/` and `tests/term_core.rs` (FMT_EXIT=1 before any edit). Keep hand formatting consistent with
  the surrounding code instead.
- Manual GUI check (badge colour, the `durable` list on screen) needs a Wayland session and is the owner's
  local test — the PR body must say so; do not claim it.

## 6. Risks

- **Lock ordering.** `live_remote_specs` locks each pane mutex. It is called from the palette-open action only
  (D2/§4.5), never from `view`. Two panes cannot deadlock here (sequential, one at a time), but a future caller
  inside the render path would.
- **Test hermeticity.** `src/session.rs`'s own tests run against the **real** `~/.local/share/ghostwave` (only
  `ui_session::test_lock` — in the *bin* test binary — redirects `XDG_DATA_HOME`). So the lib tests must use
  unique names, must assert with `contains`, and must clean up; and the palette must never read the disk in a
  test path (that is what D2 buys).
- **Prefix parsing.** `lower == "durable" || lower.starts_with("durable ")` must be checked before the generic
  fuzzy path, and `"ds "` must stay a separate prefix (a query `ds` with no space keeps falling through, as it
  does today). A sloppy `starts_with("ds")` would swallow `dsomething`, and `starts_with("durable")` would
  swallow a future `durable-x` command.
- **Category strings are fuzzy-matched**, so renaming `Remote` → `Remote — durable` / `Remote — not durable`
  changes what a query like `remote` matches. No test asserts on those categories today; the new tests do.
- **Discoverability, not enforcement.** After this change the palette *shows* the difference and offers the
  durable alternative; it does not stop anyone choosing plain `ssh`. Making `ssh` durable-by-default is a
  product decision that belongs to the owner — out of scope here (see §7).
- Badge colour choice reuses existing theme roles (`search_match`, `tab_inactive_text`), so no theme file,
  `Config` or `Theme` change is needed and themes without the role cannot regress.

## 7. The `ssh` question — closed by the owner (§0.1)

Both commands stay: `ssh <host>` keeps spawning a plain ssh pty, `ds <host>` keeps spawning the durable one.
The plan only *labels* them and offers the durable twin next to a plain `ssh <host>` when a durable session for
that host is already known (§3 D3). No command is renamed, removed, or made durable by default, and no
`ssh-plain` alias is introduced — the owner has explicitly said `ds` works and the gap is discoverability.
