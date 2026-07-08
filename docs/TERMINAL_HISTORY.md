# dd Terminal — Scrollback History Persistence Across Freeze/Restore

Status: implemented (this wave). Companion to the engine-level freeze/restore.

## The problem

dd already does engine-level freeze/restore: closing a single-shell workspace window checkpoints the
guest process tree (`ddcli workspace checkpoint`), and reopening resumes it (`launch --restore`). But the
**terminal's on-screen history was lost** — the reopened VTE widget was brand new, so the user saw only
the engine's `[restore]` lines above a resumed prompt, not the screen/scrollback they had before. VTE's
internal grid + scrollback ring is opaque and is *not* part of the guest process image the engine
checkpoints, so the two must be persisted separately.

## What VTE lets you recover (and what it doesn't)

VTE 0.8 (gtk4-rs `vte4`, C lib vte-2.91-gtk4 ≥ 0.72) text-extraction surface:

- `get_text_format(Format::Text)` — the **visible screen** as plain text. Always available (no version
  gate).
- `get_text_range_format(Format, start_row, start_col, end_row, end_col)` — an arbitrary row range as
  plain text, i.e. **the whole scrollback + screen**. Gated behind the `v0_72` cargo feature (now
  enabled in `dd-gui/Cargo.toml`; the nix devShell ships VTE 0.84 so the symbol is present).
- The `vadjustment()` spans the entire buffer: `[lower, upper)` maps 1:1 to text rows, so dumping
  `row 0 .. upper` captures all retained history.

What you **cannot** recover from VTE:
- **Styling / colors / attributes.** `Format::Text` is plain text; `Format::Html` exists but round-tripping
  HTML back into a terminal isn't meaningful. Replayed history is therefore monochrome (dim), which reads
  correctly as "this is old, inert scrollback".
- **Cursor position / alt-screen state / live TUI framebuffers.** A frozen `vim`/`htop` screen dumps as
  whatever text was visible; on restore it's shown as inert history, and the resumed (or fresh) shell
  draws its own live screen below.
- **Exact wrap points of reflowed lines** — text comes out logically wrapped; close enough for history.

## The approach chosen

On **window close** (`save_session`, in the existing `close_request` path, run *before* the engine
checkpoint + process kill):
1. Walk each tab's widget tree into a `dd_term_core::session::PaneNode` tree (leaf = terminal, split =
   `GtkPaned`), recording each pane's cwd (from OSC 7) and split ratios.
2. For each terminal leaf, dump its full scrollback via `get_text_range_format` over the whole
   `vadjustment` range, clamp to the most-recent 5 000 lines (`session::clamp_history`), and write it to
   `<storage_dir>/session/hist-<n>.txt`.
3. Serialize the tab/split layout (referencing those history files) to
   `<storage_dir>/session/layout.conf`.

On **window open** (`restore_session`):
1. Load the saved `Session`. If it has tabs, rebuild them (tabs + nested `GtkPaned` splits + ratios);
   otherwise open a single fresh shell as before.
2. For each terminal leaf, read its history file and, **before spawning the shell**, `feed()` the
   replay bytes into the fresh VTE. `session::replay_bytes` normalizes `\n`→`\r\n` (VTE is a raw
   terminal), trims trailing blanks, and appends a dim `── restored history ──` separator so the user
   can see where the replayed scrollback ends and the live session resumes.
3. The **first** leaf of the first tab still consumes the engine `--restore` (checkpoint resume) if a
   checkpoint exists, so history replay and process-tree resume compose: old screen on top, then the
   resumed tree's output below the separator.

Because history persistence is independent of the engine checkpoint, it works for the cases the checkpoint
can't cover today (multi-tab / split workspaces): those reopen with fresh shells but with their prior
scrollback shown above each prompt, and their layout intact.

## Format

`<storage_dir>/session/layout.conf` — prefix (Polish) notation, whitespace-tokenized, dependency-free:

```
# dd session layout
version 1
tab shell%201 leaf /root/my%20project hist-0.txt
tab build hsplit 0.5000 leaf - - vsplit 0.3000 leaf /tmp - leaf - -
```

- `tab <title> <node>` — one line per tab; `<node>` is a self-delimiting pre-order tree.
- `leaf <cwd> <histfile>` — `-` sentinel for "none"; values percent-escape spaces/`%`.
- `hsplit|vsplit <ratio> <a> <b>` — binary split, arity-2 so no parentheses are needed.

History files sit alongside as `hist-<n>.txt` (plain UTF-8). The whole `session/` dir is rewritten on
each save and removed when a workspace has no shell tabs.

All of this — layout round-trip, history replay-byte generation, cwd-URI decoding, and clamping — is pure
logic in `dd-term-core` and covered by unit tests (`cargo test -p dd-term-core`), so it's verified
headlessly on any host without a GUI.

## Follow-ups / honest limits

- Replayed history is monochrome by design (see above). If colored history mattered, one could persist
  `Format::Html` and render it to a non-VTE banner widget, but that's a much bigger lift for little value.
- Split-ratio restore is applied via a short deferred timeout once the `GtkPaned` is allocated; on very
  fast reopen the panes settle to their saved ratio a frame or two after paint.
- Live-reload of `term.conf` re-applies the config's global scrollback to open terminals; a per-workspace
  scrollback cap is re-applied on the next new tab, not retroactively on reload.
