# Husklet status — 2026-07-30

Measured, not inferred. Every number here came from a real run; where something could not be observed it says
so. Git holds the history — this file holds only current state and what is genuinely open.

## Suite state

| crate | passing / failing |
|---|---|
| `hl-gl` | 513 / 0 |
| `hl-gpu` | 246 / 0 |
| `hl-gpu-wgpu` | 257 / 0 |
| `hl-compositor` (smithay adapter) | 258 / 0 |
| `hl-cuda` | 135 / 0 |
| `hl-vulkan` | 202 / 0 |
| guest driver shims (5 workspaces) | 74 / 51 / 41 / 11 / 3, all 0 failing |
| `e2e` conformance suite | 53 / 12, no false greens |

`cargo fmt --all -- --check` clean. Design-lint 109 errors, down from 125; it is not a CI gate — CI runs
`cargo test --workspace` directly, while `make test` depends on it.

`make shims` reaches the five guest-driver shim workspaces, which no root cargo command can see. Two red
tests had been invisible there.

## Chrome, as last measured

From a real `google-chrome` 150 run in workspace `chrome-arm` against a live page, 11 of 13 acceptance
checks pass: GPU compositing enabled, WebGL 2.0 pixel-correct, canvas accelerated, not a software renderer,
zero GPU-process crashes, warmed frame cadence 60.003 Hz with p50/p90/p99 of 16.7/16.7/16.8 ms and no
spikes.

The two failures:

- **`post_navigation_frame_cadence`** — 56.8 Hz effective from exactly two spikes, 151 ms at frame 0 and
  50 ms at frame 2, then flawless for the remaining 117 frames. Both are near-exact integer multiples of the
  16.7 ms refresh, so this is N frames withheld one refresh at a time, not a slow allocation. Counters now
  distinguish the two candidate causes; reading them needs a windowed run.
- **`webgpu_correct`** — `requestAdapter()` resolves null. Chrome's Ozone-Wayland backend refuses Vulkan
  outright, and its WebGPU-over-GL interop path needs external-memory extensions the driver correctly does
  not advertise, because the virtual render node genuinely has no dma-buf export. Not reachable by a bounded
  change.

Guest CPU is the real slowness, not the GPU: that page took 11.3 s to load with 21 long tasks including
several over a second. That is V8 under binary translation, and no compositor work touches it.

## Blocked on one thing

The host `nix-daemon` is not running, so `nix develop`, `make app` and `make install` all fail. The plist at
`/Library/LaunchDaemons/org.nixos.nix-daemon.plist` is valid but not bootstrapped:

```sh
sudo launchctl bootstrap system /Library/LaunchDaemons/org.nixos.nix-daemon.plist
```

Everything below waits on that, because `/Applications/Husklet.app` still carries a driver bundle predating
the day's work:

- Chrome acceptance — the only thing that can clear roughly a dozen changes on paths Chrome exercises, none
  of which has been observed against Chrome
- the glmark2 per-scene matrix, whose committed baseline is 0 of 17 scenes passing
- GTK4 and `vkcube-wayland`
- a native windowed run, the only way to get a real native input-latency number and to settle the
  post-navigation hitch

The other blocker is gone: the host was missing the `x86_64-unknown-linux-gnu` Rust target, added with
`rustup target add`, so both guest architectures cross-compile once the daemon is up.

## Open questions needing a decision

- **Popup constraint.** Popups are constrained to the parent toplevel's rectangle, not the output work
  area. xdg-shell defines constraint against the work area and real menus overhang their window, so this
  bites GTK and Qt whenever they request flip or slide. The current behaviour is deliberate and test-pinned,
  presumably because each toplevel is a separate native macOS window.
- **Two `positive.md` patterns** proposed by a lint pass; the project requires explicit approval before
  appending.
- **Spotlight** indexes the build trees, which is a standing CPU cost on the host.

## Failure modes worth remembering

These account for most of what the day found, and each is a class rather than a single bug.

- **A failure logged below error level cannot print in a release build.** `hl-log` compiles `warn` and below
  out. A driver reported success on sixty presents that composed nothing, and the explanation existed only
  in a `warn` that could never appear.
- **A skip on an absent capability reads as a pass.** About 130 tests in the host executor passed with no
  GPU present, including a 240-program differential fuzzer skipping on a bare `eprintln!`. Eight cases in the
  e2e suite reported success while proving nothing.
- **A test that stops compiling stops guarding.** The orientation regression test had silently stopped
  building, which is why frames were presented mirrored without anyone noticing.
- **A driver's state is inert without a current context.** Two audits found nothing because they never called
  `eglMakeCurrent`: limits read zero, links report failure, reflection reports nothing. The whole reflection
  and upload surface was silently unexercised.
- **A stale artifact tests as though it were fresh.** Eleven test files resolved a staging directory nothing
  writes any more and exercised a driver twenty minutes old, caught only by a checksum mismatch.
- **A differential oracle agrees whenever one side does nothing.** The CPU executor ignores samplers
  entirely, draws nothing for point and line topologies, and applies neither scissor nor viewport.
