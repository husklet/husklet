# dd Terminal — Feature Audit, Bare-Minimum Spec, and Efficiency Roadmap

Status: research / roadmap input. Read-only analysis; changes nothing.
Reference sources cloned to `reference/alacritty` (v0.18.0-dev) and `reference/wezterm` (git main).

This document catalogs the terminal features of two mature emulators, compares them against dd's
**two** terminal layers, states the bare minimum a usable terminal needs, and lays out the work
required to make dd's terminal efficient. It closes with a keep-VTE-vs-build-from-scratch recommendation.

## 0. The four columns we compare

| Layer | What it is | Where |
|---|---|---|
| **Alacritty** | GPU (OpenGL) single-window emulator. Emulation = the `vte` crate (own ANSI parser) + `alacritty_terminal` grid/term model. Deliberately minimal: no images, no ligatures, no tabs/splits/mux. | `reference/alacritty/alacritty_terminal/src/*`, `reference/alacritty/alacritty/src/renderer/*` |
| **WezTerm** | GPU (WebGPU **and** OpenGL) emulator + full multiplexer. Own parser (`wezterm-escape-parser`/`termwiz`), `term` model, `wezterm-font` (HarfBuzz), `mux` (tabs/panes/domains/tmux/ssh). The maximalist. | `reference/wezterm/{term,termwiz,wezterm-escape-parser,wezterm-font,mux,wezterm-gui,bidi}/src/*` |
| **dd-term (VTE today)** | What ships. GTK4 window that embeds the mature **VTE 2.91** widget (`vte4`) for all VT emulation + GSK-GPU rendering. dd adds tabs/splits/dashboard/UX around it. | `dd-gui/src/bin/term.rs` |
| **dd-term-core (from-scratch)** | Dependency-light (std + libc + bitflags), headless-testable VT stack meant to *replace* VTE behind a future winit/wgpu GPU shell. Hand-written parser, grid, CPU renderer, PTY, input. | `dd-term-core/src/{vt,grid,render,input,font,pty/*}.rs` |

Two things to keep straight throughout:
- **dd-term (VTE)** is the shipping product. Its emulation/rendering maturity ≈ VTE's, which is very high.
- **dd-term-core** is an early greenfield core. It is *not* wired into the GUI yet (the GUI still uses
  VTE); `render.rs` produces PNGs on the CPU, and the wgpu window is future work (`lib.rs` header).

---

## 1. Feature matrix

Legend: ✅ full · ◑ partial / limited · ⚙️ present but not wired into dd's UI · ❌ absent · n/a not applicable.
"VTE" cells reflect what the vte-2.91 widget provides; dd only benefits where `term.rs` opts in.

### 1a. VT / emulation

| Feature | Alacritty | WezTerm | dd-term (VTE) | dd-term-core |
|---|---|---|---|---|
| VT100/VT220 core (cursor, ED/EL, SGR, scroll region) | ✅ | ✅ | ✅ | ✅ `vt.rs` (CUU/D/F/B, CUP/HVP, CHA/VPA, ED/EL, ECH, ICH/DCH, IL/DL, SU/SD, DECSTBM) |
| xterm compat breadth | ✅ | ✅ (widest) | ✅ | ◑ practical subset; unknown seqs skipped |
| UTF-8 decode | ✅ | ✅ | ✅ | ✅ `vt.rs` ground state, 2–4 byte |
| Wide chars (CJK, width-2) | ✅ | ✅ | ✅ | ❌ every char = 1 cell → CJK/emoji misalign |
| Combining marks / graphemes | ✅ | ✅ (`wezterm-cell`) | ✅ | ❌ no combining support |
| Alt screen (1049/47/1047) | ✅ | ✅ | ✅ | ✅ `enter_alt`/`leave_alt` |
| Scroll regions (DECSTBM) | ✅ | ✅ | ✅ | ✅ `scroll_top/bot` |
| Tab stops (HTS/TBC, custom) | ✅ | ✅ | ✅ | ◑ hard-coded every 8 cols; HTS/TBC ignored |
| Origin mode (DECOM), insert mode (IRM) | ✅ | ✅ | ✅ | ❌ neither |
| DEC private modes breadth | ✅ | ✅ | ✅ | ◑ only 25/7/1049/47/1047/1048 |
| Mouse reporting (X10/normal/button/any, SGR 1006) | ✅ | ✅ | ✅ | ❌ not tracked, not encoded (see §2b) |
| Bracketed paste (2004) | ✅ | ✅ | ✅ | ◑ `encode_paste` exists but mode never parsed from stream |
| Cursor-key app mode (DECCKM) | ✅ | ✅ | ✅ | ◑ `CursorKeys` type exists but parser never sets it |
| Focus events (1004) | ✅ | ✅ | ✅ | ❌ |
| Bell (audible/visual) | ✅ | ✅ | ✅ (audible off) | ◑ `bell` flag only |
| OSC 0/2 title | ✅ | ✅ | ✅ | ✅ `finish_osc` |
| OSC 7 cwd | ✅ | ✅ | ⚙️ VTE emits `current-directory-uri`; dd doesn't consume | ❌ |
| OSC 8 hyperlinks | ✅ (`term/cell.rs`) | ✅ | ⚙️ VTE parses; dd wires no click handler | ❌ |
| OSC 52 clipboard | ✅ (`Osc52` policy) | ✅ | ◑ VTE-dependent build flag | ❌ |
| OSC 4/104 palette set/reset | ✅ | ✅ | ✅ | ❌ |
| Colors 16 / 256 / truecolor | ✅ | ✅ | ✅ | ✅ incl ISO-8613-6 colon form |
| DEC line-drawing (ESC ( 0) | ✅ | ✅ | ✅ | ✅ `dec_graphic` full VT100 set |
| Sixel images | ❌ | ✅ `terminalstate/sixel.rs` | ◑ VTE build-flag dependent | ❌ |
| Kitty graphics protocol | ❌ | ✅ `terminalstate/kitty.rs` | ❌ | ❌ |
| iTerm2 inline images | ❌ | ✅ `terminalstate/image.rs` | ❌ | ❌ |
| Synchronized output (DCS/DEC 2026) | ✅ (`event_loop.rs`) | ✅ (`SynchronizedOutput`) | ◑ VTE ≥0.72 | ❌ |
| Reflow / rewrap on resize | ✅ (`grid/resize.rs`) | ✅ | ✅ | ❌ resize truncates/pads top-left only |
| Cursor styles (DECSCUSR) + blink | ✅ | ✅ | ✅ (block+blink set) | ❌ block only, no DECSCUSR |
| Device reports (DA/DA2/DSR/CPR) | ✅ | ✅ | ✅ | ❌ no replies → apps that query may stall |
| Title stack (22/23 t) | ◑ | ✅ | ✅ | ❌ |

### 1b. Rendering / performance

| Feature | Alacritty | WezTerm | dd-term (VTE) | dd-term-core |
|---|---|---|---|---|
| GPU rendering | ✅ OpenGL 3.3 / GLES2 (`renderer/text/glsl3.rs`,`gles2.rs`) | ✅ WebGPU (`termwindow/webgpu.rs`,`shader.wgsl`) + OpenGL | ✅ via GTK GSK (Vulkan/GL/Metal-Cairo) | ❌ CPU only (`render.rs`); wgpu shell is future |
| Glyph atlas / caching | ✅ `renderer/text/{atlas,glyph_cache}.rs` | ✅ `glyphcache.rs`,`shapecache.rs` | ✅ (VTE) | ❌ 8×8 bitmap, no atlas |
| Damage / dirty tracking | ✅ `LineDamageBounds` | ✅ | ✅ (VTE) | ❌ full re-render each frame |
| Frame pacing / vsync | ✅ | ✅ (`colorease`, cursor ease) | ✅ (GTK frame clock) | ❌ |
| Real font / rasterizer | ✅ crossfont (FreeType/CoreText/DWrite) | ✅ `wezterm-font` FreeType/CoreText/GDI | ✅ Pango/HarfBuzz (VTE) | ❌ embedded 8×8 ASCII-only bitmap `font.rs` |
| Font ligatures | ❌ | ✅ HarfBuzz `shaper/harfbuzz.rs` | ❌ (VTE is cell grid, no shaping) | ❌ |
| Subpixel / grayscale AA | ✅ | ✅ | ✅ (Pango) | ❌ 1-bit, no AA |
| Bold / italic / faint glyphs | ✅ | ✅ | ✅ | ◑ bold→brighten color only; no italic glyph |
| Box-drawing / powerline custom glyphs | ✅ (`renderer/rects.rs`) | ✅ `customglyph.rs` | ✅ (VTE) | ◑ Unicode stored, but font can't draw them |
| Emoji (color) | ◑ | ✅ | ✅ | ❌ |
| Scrollback | ✅ ring (`grid/storage.rs`) | ✅ | ✅ 10k lines (`set_scrollback_lines`) | ❌ **no history** — scrolled rows discarded |
| Scrollback search | ✅ `term/search.rs` (regex) | ✅ | ⚙️ VTE `search_set_regex` exists; dd wires no UI | ❌ |
| BiDi | ◑ | ✅ dedicated `bidi/` crate | ✅ (VTE) | ❌ |
| IME / dead keys / preedit | ✅ | ✅ | ✅ (GTK IMContext) | ❌ |

### 1c. UX / product

| Feature | Alacritty | WezTerm | dd-term (VTE) | dd-term-core |
|---|---|---|---|---|
| Tabs | ◑ native macOS window-tabs only | ✅ | ✅ iTerm2-style equal-width, ⌘T | n/a (GUI concern) |
| Splits / panes | ❌ | ✅ | ✅ GtkPaned, ⌘D, collapse-on-exit | ◑ `layout.rs` has a pane tree model (not GPU-wired) |
| Built-in multiplexer (tmux-like) | ❌ | ✅ `mux/` (tabs/panes/domains, client+server) | ❌ | ❌ (`workspace.rs` is container-workspace, not a mux) |
| tmux control-mode integration | ❌ | ✅ `mux/src/tmux.rs` | ❌ | ❌ |
| SSH / serial domains | ❌ | ✅ `wezterm-ssh`, `mux/src/ssh.rs` | ❌ | ◑ container PTY via `DdJitPty` (different axis) |
| Session restore / persistence | ❌ | ✅ (mux server survives GUI) | ❌ | ⚙️ `WorkspaceStore` persists workspace defs |
| Config format | TOML + live reload | Lua + live reload | ❌ hard-coded in `term.rs` | ❌ |
| Keybindings (configurable) | ✅ `config/bindings.rs` | ✅ `inputmap.rs` | ◑ fixed ⌘T/⌘D/⌘W + copy | ✅ key→bytes only (`input.rs`) |
| Copy mode (keyboard selection) | ✅ vi-mode `vi_mode.rs` | ✅ `overlay/copy.rs` | ❌ (mouse selection only) | ❌ |
| URL / hyperlink click | ✅ hints | ✅ | ◑ VTE detects; dd click-handler minimal | ❌ |
| Search UI | ✅ | ✅ | ❌ | ❌ |
| Quick-select / hints | ✅ hints (regex→keyboard) | ✅ `overlay/quickselect.rs` | ❌ | ❌ |
| Command palette | ❌ | ✅ `commands.rs` | ❌ | ❌ |
| Quake / dropdown | ❌ | ✅ | ❌ | ❌ |
| Notifications / OSC 9 / 777 | ◑ bell | ✅ `wezterm-toast-notification` | ◑ VTE bell signal | ❌ |
| Selection + clipboard | ✅ | ✅ | ✅ copy/paste, scroll damping | ❌ no selection model |
| Ligature-aware cursor / mouse | n/a | ✅ | ✅ | ❌ |

---

## 2. What dd is MISSING vs Alacritty & WezTerm

### 2a. Missing even *with* VTE (i.e. gaps in the shipping product `term.rs`)

These are product/UX gaps, not emulation gaps — VTE already emulates well; dd just hasn't wired the UX:

1. **No multiplexer / session persistence.** WezTerm's headline feature (`mux/`): a server that owns
   panes and survives the GUI, tmux control-mode, remote domains. dd has *container workspaces*
   (`workspace.rs`) — an orthogonal, arguably more valuable axis — but no terminal-session mux.
2. **No search UI.** VTE exposes `search_set_regex`/`search_find_next`; `term.rs` never surfaces it.
3. **No copy mode / keyboard selection.** Only mouse selection + ⌘C (`copy_clipboard_format`).
4. **No configurability.** Font ("Menlo 12"), palette, scrollback (10k), cursor, keybindings are all
   hard-coded in `style_terminal`/`term.rs`. No config file, no live reload. Alacritty=TOML, WezTerm=Lua.
5. **No hyperlink click / URL hints / quick-select.** VTE parses OSC 8 and can regex-match URLs; dd
   wires no activation. No hint overlay, no command palette, no quake mode.
6. **No inline images beyond whatever VTE build enables.** Sixel is a VTE build-flag; kitty/iTerm2 are
   absent. WezTerm supports all three.
7. **No ligatures.** VTE is a fixed cell grid and does not shape ligatures; WezTerm does (HarfBuzz).
8. **No OSC 7 cwd consumption** (open-new-tab-in-same-dir), no notifications (OSC 9/777).

### 2b. Missing in the from-scratch `dd-term-core` path

Everything VTE gives for free that a home-grown stack must re-implement. `dd-term-core` today is a
solid *practical VT subset* but is far from parity. Concrete gaps, by file:

**Emulation (`vt.rs`):**
- **No scrollback.** `grid.rs` holds only the visible screen; `scroll_up`/`scroll_region_up` discard
  the top row. There is no history ring buffer at all — a fundamental miss.
- **No wide-char / grapheme width.** Every `char` occupies one cell (`put_char`), so CJK, wide emoji,
  and combining sequences corrupt the grid alignment.
- **Input-mode desync.** The parser explicitly ignores DECCKM (cursor-key app mode), bracketed-paste
  mode (2004), and mouse modes ("input-side: ignore on output"), but `input.rs`/`encode_paste` have no
  channel to *learn* those modes from the stream. Result: arrow keys in vim/less and safe paste can't
  work correctly because nothing sets `CursorKeys`/`bracketed`.
- **No mouse reporting at all** (neither mode tracking nor encoding). `input.rs` has zero mouse support.
- **No device replies** (DA/DA2/DSR/CPR): apps that probe the terminal get silence and may hang.
- **No reflow on resize** (`resize` truncates/pads the top-left rectangle; no rewrap of long lines).
- **Missing modes/ops:** origin mode (DECOM), insert mode (IRM), DECSCUSR cursor styles, focus events
  (1004), reverse-screen (?5), HTS/TBC tab stops, title stack, OSC 4/7/8/52, sixel/kitty/iTerm2 images,
  synchronized output (2026), APC.

**Rendering (`render.rs` + `font.rs`):**
- **Font is an 8×8 ASCII-only bitmap** (`FIRST=0x20..LAST=0x7e`). Any non-ASCII — including the
  box-drawing glyphs the *parser itself produces* via `dec_graphic`, plus all UTF-8, emoji — renders as
  the hollow-box fallback. No real font, no shaping, no AA, no bold/italic glyphs (bold only brightens
  color), no glyph atlas, no damage tracking (full frame re-raster every time), no GPU.

**UX:** no selection model, no clipboard, no tabs/splits wired to a GPU surface (`layout.rs` has the
pane-tree data model but nothing renders it), no config, no search — none of it exists yet.

---

## 3. Bare minimum — what a terminal MUST have to be usable day-to-day

Ordered. "Table stakes" = a shell + vim/htop/git/ssh are unusable or visibly broken without it.
"Nice to have" = expected by power users but not blocking.

### Table stakes (must ship before it's a real terminal)
1. **VT100/VT220 + xterm core**: CUP/CUU/CUD/CUF/CUB, ED/EL, SGR (bold/underline/reverse + fg/bg),
   DECSTBM scroll regions, IL/DL/ICH/DCH, autowrap, CR/LF/BS/HT. *(core has this)*
2. **UTF-8 decode + correct wide-char (width-2) and combining-mark handling.** Without width awareness,
   any CJK/emoji/box-heavy TUI misaligns. *(core: decode ✅, width ❌)*
3. **Alt screen + scroll regions** so vim/less/htop don't corrupt the shell screen. *(core ✅)*
4. **Scrollback buffer** (ring, ≥1k lines) with wheel scroll. Non-negotiable. *(core ❌)*
5. **Resize + reflow**: TIOCSWINSZ on the PTY *and* rewrapping long lines on width change. *(core: winsize ✅, reflow ❌)*
6. **Truecolor + 256-color + 16 ANSI**, with erase-uses-current-bg. *(core ✅)*
7. **Correct keyboard encoding**: control bytes, Alt-as-ESC, arrows/Home/End/PgUp/Del, function keys,
   **DECCKM app-cursor mode**, back-tab. *(core: mostly ✅, but app-mode not driven by parser)*
8. **Mouse reporting** (at least SGR 1006 + normal 1000) — required by tmux, vim, htop click, mc.
   *(core ❌)*
9. **Bracketed paste** with mode actually tracked from the stream (2004). *(core: encoder ✅, mode-tracking ❌)*
10. **Selection + system clipboard** (copy/paste). *(core ❌)*
11. **Cursor show/hide (DECTCEM)** and at least block + bar shapes (DECSCUSR). *(core: show/hide ✅, shapes ❌)*
12. **A real font renderer**: monospace glyph rasterization covering the whole BMP incl. box-drawing,
    with bold and grayscale AA. *(core ❌ — 8×8 ASCII bitmap)*
13. **Device attribute / cursor-position replies** (DA, DSR/CPR) so probing apps don't hang. *(core ❌)*
14. **DEC line-drawing charset.** *(core ✅)*

### Nice to have (expected, not blocking)
- OSC 8 hyperlinks + click, OSC 7 cwd, OSC 52 clipboard, title stack.
- Synchronized output (2026) — big flicker win for fast TUIs, but apps degrade gracefully without it.
- Cursor blink, focus events (1004), configurable keybindings/colors/font, search UI.
- Italic/faint glyphs, ligatures, sixel/kitty/iTerm2 images, BiDi, IME (needed for CJK input),
  quick-select/hints, tabs/splits, multiplexer.

---

## 4. Efficiency roadmap (making dd's terminal fast)

Priorities assume the **from-scratch GPU path** (if dd stays on VTE, most P0/P1 are already handled by
VTE/GSK — flagged per row). P0 = required for a smooth 60fps+ terminal; P1 = important under load;
P2 = polish/scale.

| # | Feature | Prio | Why | VTE already does it? |
|---|---|---|---|---|
| E1 | **GPU glyph-atlas renderer** (upload rasterized glyphs once to a texture atlas; draw cells as textured quads via wgpu). Ref: Alacritty `renderer/text/atlas.rs`, WezTerm `glyphcache.rs`. | P0 | The single biggest perf lever; CPU per-pixel raster (`render.rs`) can't hit interactive rates on a real grid. | ✅ (GSK) — only relevant if dropping VTE |
| E2 | **Damage / dirty-line tracking** — re-render only changed cells/lines. Ref: Alacritty `LineDamageBounds`. | P0 | Full-grid redraw every frame wastes GPU + battery; TUIs touch few cells/frame. | ✅ (VTE) |
| E3 | **Batched draw** — one instanced draw call per frame (bg quads + glyph quads), not per-cell. Ref: WezTerm `quad.rs`. | P0 | Draw-call overhead dominates otherwise. | ✅ (VTE) |
| E4 | **Async PTY read → parse pipeline** — read on an epoll/kqueue-driven loop into a byte queue; parse off the UI thread; the GUI already polls `master_fd`. Coalesce bursts before rendering. | P0 | Keeps input latency low and prevents a `yes`-flood from freezing the UI. | ◑ (VTE reads on GLib loop; parse is VTE's) |
| E5 | **Scrollback ring buffer** — fixed-capacity `VecDeque<Row>` with O(1) push/evict, decoupled from the visible viewport. Ref: Alacritty `grid/storage.rs`. | P0 | Prerequisite for scrollback at all; naive `Vec` reallocation would stutter. | ✅ (VTE) |
| E6 | **Frame pacing / vsync** — render at most once per display refresh; coalesce many PTY writes into one frame; throttle to the monitor's cadence. | P1 | A fast writer must not drive >60 renders/sec; caps CPU. | ✅ (GTK frame clock) |
| E7 | **Synchronized output (DEC 2026)** — buffer between BSU/ESU and swap atomically. Ref: WezTerm `SynchronizedOutput`, Alacritty `event_loop.rs`. | P1 | Eliminates tearing/flicker on full-screen TUIs; lets apps hint frame boundaries. | ◑ (VTE ≥0.72) — genuine gap in core |
| E8 | **Grapheme / wide-char segmentation** — cluster codepoints (unicode-width + combining) into cells before writing the grid. | P1 | Correctness *and* perf: pre-segmenting avoids per-frame width recompute; blocks E1 correctness. | ✅ (VTE) — genuine gap in core |
| E9 | **Shaping cache** (only if pursuing ligatures) — cache shaped runs per (text,font). Ref: WezTerm `shapecache.rs`. | P2 | Reshaping every frame is expensive; unnecessary if no ligatures. | n/a (VTE no ligatures) |
| E10 | **Reflow on resize** without full re-parse — rewrap stored logical lines. Ref: Alacritty `grid/resize.rs`. | P1 | Resizes must be O(changed) not O(scrollback); also a correctness gap. | ✅ (VTE) — genuine gap in core |
| E11 | **Rect/underline/cursor pass** batched separately from glyphs. Ref: Alacritty `renderer/rects.rs`. | P2 | Cheap decorations without per-cell branches. | ✅ (VTE) |
| E12 | **Snapshot-for-render** — the grid is `Clone` (`grid.rs`); take an immutable snapshot so the parser keeps running while a render thread draws. | P2 | Removes UI/parse contention under load. | ✅ (VTE) |

**Bottom line for the core path:** the genuine, VTE-*independent* efficiency gaps are E7 (sync output),
E8 (grapheme width), E10 (reflow). Everything else (E1–E6, E11–E12) only matters *because* the core
would drop VTE/GSK and must rebuild the GPU + damage + pacing stack that GTK gives for free today.

---

## 5. Recommendation

**Keep VTE for the shipping product now; treat `dd-term-core` as a deliberate, staged bet — not the
near-term path.** Concretely:

### Why keep VTE (near/mid term)
- VTE gives dd, *today*, essentially all of §1a/§1b for free: full xterm/VT220 emulation, UTF-8 +
  wide + combining + BiDi + IME, alt screen, scroll regions, mouse, bracketed paste, truecolor,
  reflow, 10k scrollback + regex search engine, OSC 7/8, cursor shapes, damage tracking, and **GSK GPU
  rendering** (Vulkan/Metal/GL). Re-implementing this to parity is *years* of VT edge-case work
  (Alacritty and WezTerm each represent many person-years, and Alacritty still punts on images,
  ligatures, tabs, splits, and a mux).
- The product's *actual* differentiators are the **container-workspace** integration (`workspace.rs`,
  `DdJitPty`, per-workspace dashboard) and UX (tabs/splits/⌘-keys) — none of which require owning the
  emulator. The highest-ROI work is in §2a (search UI, copy mode, config + live reload, hyperlink
  activation, OSC 7 new-tab-in-cwd, a session/mux layer over workspaces), all buildable on VTE.

### Cost of keeping VTE
- **Couples dd to GTK4 + the GObject/Pango stack.** That is already a first-class dependency (the whole
  GUI is GTK4), so the marginal cost is low — but it blocks any future non-GTK shell (winit/wgpu),
  ligatures (VTE won't shape), kitty/iTerm2 images, and fine-grained control over
  checkpoint/restore of live terminal state for dd's CRIU/workspace story.

### When the from-scratch core earns its keep
Invest in `dd-term-core` only if/when a concrete requirement forces it:
- a **non-GTK** GPU shell (winit/wgpu) is wanted for portability or startup/footprint, **or**
- **deep workspace/checkpoint integration** needs to serialize/restore exact grid+scrollback state
  (VTE's internal state is opaque), **or**
- product wants **ligatures / kitty-graphics / a custom render pipeline** VTE can't give.

If that bet is taken, sequence it so it's never worse than VTE at any milestone:
1. **Correctness parity first** (behind a feature flag, still shipping VTE): close the §2b emulation
   gaps — scrollback ring (E5), wide/grapheme width (E8), reflow (E10), mouse reporting + mode-tracking
   feedback to `input.rs`, device replies, sync output (E7). Keep the headless PNG oracle
   (`render.rs` + `CpuRenderer`) as the differential test harness against a VTE/xterm reference.
2. **Real font + GPU** (E1–E3): replace the 8×8 bitmap with a FreeType/CoreText rasterizer + wgpu glyph
   atlas; add damage tracking + batched draw + frame pacing (E2/E3/E6).
3. **UX on the new surface**: wire `layout.rs` panes to the GPU window, selection/clipboard, then the
   §2a product features.
Only flip the GUI default from VTE to core once the flagged core beats VTE on the differential suite.

**Net:** VTE is the pragmatic base for at least the next several releases; put roadmap energy into the
UX/mux/config gaps of §2a. `dd-term-core` is a sound long-term insurance policy and the right home for
tight workspace/checkpoint integration, but its §2b gap list (starting with *no scrollback*, *no wide
chars*, *ASCII-only font*, *no reflow*, *no mouse*) means it is not close to replacing VTE today, and
should graduate only behind a feature flag proven at parity by the existing headless oracle.
