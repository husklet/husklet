# Husklet instructions

These rules define the durable architecture, safety, coding, testing, and delivery
standards for Husklet and its integrated C execution engine. Apply them to every new package and improve
nearby code when doing so preserves behavior and remains within scope.

The retired C engine in `../engine` is a read-only behavioral and performance oracle
during migration. Husklet is the active repository and owns the integrated C engine, Rust control plane, containers,
workspaces, terminal, and desktop application. Do not add GPU, graphics translation,
surface, compositor, CUDA, OpenGL, Vulkan, or Wayland implementation back into this
repository. Never edit `../engine` while studying it.

Everything before "Mission" is process rules, and almost every one exists because
it cost real work. If you are about to **measure** anything, or to **count** what
a suite reported, read "Counting and measurement" — it absorbs the seven sections
that used to carry this scattered, among them "Balance the arm order", "A control
that merely seems unaffected is not a control", "`bench --results` is a resumable
ledger", "Identical source does not mean an identical binary" and "Reading a
profile". "A C change reaching the binary is a separate claim, and it has its own
check" stayed separate and belongs beside them. If you are about to **commit**,
read "What green means" and its subsections. Everything from "Mission" onward is
durable architecture and changes rarely.

**A counter governed by the policy cannot decide the policy.** A lane proposed
replacing a flip budget with a threshold on `direct_declined`, and the counters
appeared to confirm it: the plain guest finished at 1 declined site at every
budget, sqlite at 4 to 26. But `malloc` **alone** declines 17 sites on the
plain guest. It reads 1 in the whole sequence only because the budget's hold
has already fired and suppressed direct mode — and suppressed direct mode
cannot guard-fault, which is the sole entrance to the set. The evidence for
the replacement was produced by the thing being replaced.

Implementing it settled what the counter could not: `admitted` fell 255,303 to
1,228, the sqlite phase regressed 1.74x, and both guests latched inside
`malloc` and lost direct authority for the rest of the process. Before keying
a decision on a counter, ask what writes to it and whether the current policy
gates that write.

## Reading code: CodeGraph first

This repository is indexed by CodeGraph (`.codegraph/` at the root). Reach for it
**before** grep, find, or opening files, both to answer a question and before
editing a symbol. One `codegraph_explore` call returns the verbatim,
line-numbered source of the matching symbols grouped by file — safe to edit from,
and equivalent to having read them — plus the call path among them and a blast
radius naming every caller and the tests that cover each symbol. Prefer the MCP
tool `codegraph_explore`; `codegraph explore "<names>"` in a shell prints the same
output when the tool is unavailable.

The blast radius reports **`no covering tests found`** per symbol. Treat that as a
first-class signal: it names the places where a green suite proves nothing, which
is where this codebase has repeatedly hidden defects.

Two failure modes, both observed:

- **Query precise symbol names, two to four at a time. Never bare filenames.**
  A filename matches repo-wide — `pool.rs` pulls in unrelated container and
  launcher files and spends the whole budget on them.
- **Output is budget-truncated and truncation is silent.** A broad query can drop
  the symbol you asked about and leave it visible only in the blast radius. If the
  source you needed is not in the reply, ask again with fewer, narrower names
  rather than assuming it does not exist.

Do not re-open a file whose source CodeGraph already returned.

`no covering tests found` is a lead, not a verdict. It has misfired repeatedly,
including on symbols whose removal reddens both unit and integration tests.
Confirm a gap by mutating the symbol and watching the suite, which is the standard
this repository requires for a coverage claim anyway.

## What "green" means

The corpus is not the gate. The `nix flake check` verification derivation runs
`cargo test --workspace --all-targets`, which covers the Rust library tests and
the native-package integration tests; the corpus reaches neither.
Eleven stale assertions once survived because every lane ran the corpus and a
targeted `cargo test -p <pkg>` and nobody ran the workspace.

Before committing, run `cargo test --workspace --lib --bins` and
`cargo test -p hl-native --all-targets` — about a minute, and it catches that
whole class. Run them in debug or inside `nix flake check`: under `--release`,
`hl-log`'s verbose tests compile out and the daemon tests need
`HL_ALPINE_ARCHIVE`, so both report spurious failures.

### Two green branches can merge to a red tip

`cargo test --workspace --lib --bins` on each branch proves nothing about the merge.
One lane turned `Signal` from a seven-variant enum into a `1..=64` value type
while another added a test spelling `Signal::Kill`; both were green alone and
the tip did not compile. Git reported no conflict, because there was none —
the conflict was semantic.

So run the gate **after** merging, not only before, and run it on the tip you
are about to push. A `cargo check --workspace --all-targets` is a minute and
catches this whole class.

**Green on the pre-merge commit is not green on what you push.** Six proven arms
on your own SHA are evidence about your change in isolation. The thing you push
is the merge, and the merge is precisely where a conflict with the other lanes'
week would first appear. Re-run every arm on the merged tip, including the ones
you already have.

### Push the SHA you gated, not the branch name

On a `main` that nine lanes merge into continuously, "the tip you are about to
push" moves while you are gating it. A lane gated `653e2595b`, watched nine
arms go green over twenty minutes, ran `git push origin main`, and shipped
`0c84f1909` -- two sibling merges had landed in the gap, the second carrying
four ungated C files. Nothing warned it; `git push` reports the range it sent,
which is the first and only place the extra commits appear, and by then they
are on the remote.

    git push origin <gated-sha>:main

pushes exactly what was verified and **fails** if the remote has moved past it,
which is the outcome you want -- it turns a silent widening of the push into a
non-fast-forward you must go and look at. Re-read `git rev-parse main` right
before pushing if you use the branch name anyway, and compare it to the commit
you gated.

### Gate the diff, not the repository, under a mechanical rule

The full gate is about twenty minutes, and on a `main` that moves every ten a
lane can spend three of them in a row proving things its diff could not have
broken. One lane ran `cargo test --workspace --lib --bins` three times because
a YAML comment changed. The rule that earns the saving without losing anything:

- **Workflow-only means `git diff --name-only <base>..<tip>` touches nothing
  outside `.github/`.** Not "mostly workflow", not "workflow plus one comment".
  A single path outside and you run the full gate. The predicate is mechanical
  precisely so it cannot be argued with at 2am.
- For those commits: `actionlint` over every workflow, **plus the specific
  checks the diff touches, run for real.**
- **`flake.nix` is never workflow-only.** It builds code; it gets the full gate.
- Divergence check and `git push <sha>:main` still apply. They are seconds, and
  each has caught a real mistake.

The part that is not negotiable: **a workflow commit that adds a step is a claim
that the step passes, and that claim needs the step run somewhere.** Verify the
thing you are adding on the exact tip you are adding it to. What you may skip is
re-running the suites the diff cannot reach.

### Ask of every check whether anything runs it

A gate nobody invokes cannot fail, and that is indistinguishable from a gate
that passes. `scan-build` found two real C defects here and ran in **no CI job
on any host** -- `ci.yml` never named it, and a `lib.optionalAttrs
pkgs.stdenv.isLinux` attribute is invisible to the macOS `nix flake check`. It
was found only because a lane asked whether an assertion *it had just written*
would ever be exercised. Ask that of anything you add, on the day you add it.

### What `--bins` and `--all-targets` do not reach

`--bins` is not optional. `cargo test --workspace --lib` alone runs **no** test
in a crate that has no library target, and `testing` is bin-only: `cargo test -p
testing --lib` answers `no library targets found in package testing`, silently
in a workspace-wide run. Every assertion in `testing`'s benchmark and syscall
audit gates was invisible to the command this file told everyone to run.

**`--all-targets` does not run a required-features test either.**

`cargo test -p hl-native --all-targets` reports green while running **zero** of the
tests in a target declared `required-features = ["native-test-hooks"]`. Cargo skips
such a target silently, exactly as it does for the application binaries below.
`tests/namespace_transaction.rs` is one of these, and a lane discovered mid-run that
the gate command this file recommends was not exercising any of it.

That matters twice over: the same feature flag is what makes a symbol's only writer
test-only, which is the defect `c-test-only-state` exists to catch. So the build
configuration that hides the writer is also the one whose tests do not run by
default. Add `--features native-test-hooks` as a second invocation when you touch
anything behind that flag, and say which of the two you ran.

**And `--all-targets` does not reach the application.**

`src/apps/husklet` declares `[[bin]] husklet` with `required-features = ["gui"]`, and its
`runtime` feature is off by default. Cargo **skips a target whose required features are
unmet without printing anything**, so `cargo clippy --workspace --all-targets` covers the
`hl` library's default surface and none of the ~10,800 lines the signed application is
built from. A lane can rename a shared type, watch the workspace go green, and have broken
the product. That is the permanently-armed form of the merge hazard above.

Making it visible cost four lines of `flake.nix`: the Linux dev shell now carries `gtk4`,
`librsvg` and `vte-gtk4` exactly as the Darwin one does, all substitutable (~257 MiB, no
source builds). CI therefore runs the two feature-gated Clippy commands explicitly.
Run the same `cargo clippy -p husklet --all-targets --features runtime` and
`cargo clippy -p husklet --all-targets --features gui` commands inside `nix develop`
when you only want the application answered.

Linking and **running** the GTK app is no longer on the list of things Linux cannot do —
see the next subsection. What a Linux run still does not cover: the
`#[cfg(target_os = "macos")]` arms in `bin/host/{environment,pty,appearance}.rs` and
`runtime/process.rs`, the objc2 title-bar and appearance code, `package/bundle.sh` (which
refuses any host but Darwin, deliberately — there is no Linux packaging step to write), and
everything in `.github/workflows/release.yml` — bundling, code signing, notarization, the
DMG. The macOS arms are still only compiled by CI on `macos-26`.

### Running the GTK application headless on Linux

This box has no monitor and GTK exits when it cannot open a display, so for a long time the
application could be type-checked here and never started. It starts. The Linux dev shell
carries `xorg-server` and `xvfb-run` beside the `gtk4`/`vte-gtk4`/`librsvg` it already had,
and the app's own `HL_TERM_SHOT` hook renders the window through GTK's snapshot pipeline
and writes a PNG — no screen capture, no compositor, no X client tooling:

```text
nix develop -c env \
  HL_TERM_VIEW=manager HL_TERM_SHOT=/var/tmp/manager.png HL_TERM_SHOT_MS=1500 GTK_A11Y=none \
  xvfb-run -a -s '-screen 0 1600x1000x24' -- ./target/debug/husklet
```

`HL_TERM_VIEW` picks the surface (`manager`, `terminal`, `newws`) and the shot fires only for
the window it names. `HL_TERM_WS=<name>` selects a workspace from `$HOME/.hl/workspaces.conf`,
and `HL_TERM_TABS=N`, `HL_TERM_SPLIT=h|v`, `HL_TERM_TYPE=<text>`, `HL_TERM_OVERVIEW`,
`HL_TERM_OVERVIEW_PAGE` and `HL_TERM_DEBUG_LOG=<path>` drive it further.

A screenshot proves a window produced pixels. It cannot say which glyphs they are, cannot
send a keystroke *while something is already running*, and cannot resize anything — so
three further hooks carry the loop the rest of the way:

- `HL_TERM_TEXT=<path>` writes what every pane is showing, as text, beside the PNG. This
  is the assertable artefact: a screenshot needs a human or an OCR pass, a grid extraction
  can be grepped for the output of a command the run typed.
- `HL_TERM_SCRIPT=<ms>:<bytes>` chunks, separated by **ASCII RS (0x1e)**, write each
  payload into a pane's tty at its own offset from launch, verbatim and with no newline
  appended. A control byte is spelled by putting the control byte in the variable, which
  is how a Ctrl-C is delivered mid-command. The separator is not printable because every
  printable candidate — `|` above all — is something a real command line contains.
- `HL_TERM_RESIZE=<ms>:<width>x<height>` resizes the **window**, in pixels. Do not
  "simplify" this to `set_size` on the panes: a pane is `hexpand`/`vexpand`, the next
  allocation overwrites the grid, and the guest goes on reporting the old geometry while
  the hook reports success. That was measured here before it was fixed.
  **Unverified as of 2026-08-21:** the tree carries two disagreeing doc comments
  for this variable -- `screenshot.rs:40` says window pixels, as above, and
  `husklet.rs:102` says "a grid geometry to apply to every pane, as
  `<ms>:<columns>x<rows>`". One of the two is wrong; the GUI owner should say
  which rather than either of us guessing from the outside.

`storybook` carries the same hook under `STORYBOOK_SHOT`/`STORYBOOK_SHOT_MS`.
Verified on x86_64 Linux at ad652522d: the manager window, the New-Workspace dialog,
a terminal window with three tabs, a vertical split and typed text queued into both
panes, and all 11 storybook stories (291 widgets).

Six things to know before you trust — or debug — this loop:

- The terminal panes **do** enter the image, and the loop reaches a live shell. The
  earlier note here said an OCI pull was out of reach and the screenshot therefore proved
  only the toolkit; that was true of a run with no workspace configured. Write one into
  `$HOME/.hl/workspaces.conf` (`name`/`image`/`arch`, three lines) and the pane's worker
  starts the per-workspace domain, pulls the image and attaches a container exec:
  measured 2026-08-21 on `naa0245` at 2b2cd63cf, ~20 s from launch to a prompt on the
  first run including the pull of `alpine:3.20`, ~5 s once the domain is warm. Point
  `HOME` at a scratch directory — `~/.hl` is shared with every other lane's tests.
- `GTK_A11Y=none` only silences an `org.a11y.Bus` warning on a box with no accessibility
  bus. It is noise, not a failure.
- **The guest's terminal echoes every typed line twice, and this is not the toolkit.**
  Measured 2026-08-21 at 2b2cd63cf against a live `alpine:3.20` workspace: every command
  line appears once as busybox `ash`'s line editor draws it and again after the newline,
  and each prompt carries a visible `^[[N;5R` — VTE's answer to the `ESC[6n` the editor
  sends, echoed back as though it had been typed. The host pty is not the culprit;
  `stty -a -F /dev/pts/N` on the live pane reports `-isig -icanon -echo`. The same
  busybox binary, extracted from the same image layer and run under `chroot` on this
  host's own kernel through a real pty, echoes each line exactly **once**. And the
  guest's own termios is not simply ignored: `stty -echo` inside the container is
  read back as `-echo` and the *second* copy stops. What is not honoured is the
  raw-mode `tcsetattr` a line editor performs immediately before the read it is about
  to do — which is what every interactive shell does on every prompt. It belongs to the
  engine's line discipline, not to `hl-gui-gtk` or the `husklet` bins.
- `cargo test -p husklet --bins --features gui` needs no display **to run**, and two of
  its tests need one **to mean anything**. Measured 2026-08-21 on this box, both ways:
  **`running 88 tests` / `88 passed` in both**, and only the display-less run prints
  `skipped: no display connection`. The count cannot tell them apart, which is why this
  has survived — `test_support::on_the_toolkit_thread` answers `false`, both callers say
  they skipped, and between them 19 scenarios do not execute. Under
  `xvfb-run -a -s '-screen 0 1600x1000x24' -- cargo test …` the skip lines are gone and
  all 88 pass. Proven non-vacuous by planting a false assertion in one of those
  scenarios: **87 passed / 1 failed under `xvfb-run`, 88 passed without a display.**
  Wrapping the CI step is a decision for whoever owns the workflow, not a repair to make
  silently — but do not read that step's green as covering the extension page or the
  extension shelf until it is.
- `TermConfig::default().font_family` is `Menlo`, which exists only on macOS. On Linux
  Pango falls back and VTE takes its cell metrics from the fallback, so the grid renders
  visibly letter-spaced until a workspace sets `font_family` to a font the host has
  (`DejaVu Sans Mono` here). That is host-local cosmetics, not a defect — do not "fix" it
  by changing the product default, which is correct for the host the app ships on.

### Work in your own worktree, and stage by path

Several lanes edit the shared checkout at once. Two things follow, both of which
have already cost work today:

- **`git add -A` in the shared tree stages other lanes' uncommitted files.** One
  lane swept another's `main.c` into its commit and caught it only on the stat.
  Always `git add <path>`, never `-A`, and read the stat before committing.
- **A dirty shared tree breaks everyone's build.** A lane found the tree would
  not compile because of an unrelated in-flight edit, and had to run its gate in
  a clean detached worktree to get a trustworthy answer. If the tree does not
  build and your diff cannot explain it, check `git status` before debugging.

Prefer your own worktree. If you must work in the shared tree, leave it clean.

### Copy a built binary in its own command, then run it once

A lane copied `release/hl-x86_64` inside the same script as its `cargo build`,
immediately after the build returned. The copy was corrupt — and **corrupt
deterministically**, identical sha256 twice, which is what made it credible. A
wrong answer that reproduces looks exactly like a right one.

It then failed as `Engine(Load(Inspect { role: Main, error: WrongArchitecture }))`
— pointing at the guest, not at the copy — and was reported as a tip-wide break
that stopped an amd64 guest loading. Rebuilding from the same commit produced a
working binary; all six commits in the suspected range ran cleanly.

So: copy in a separate command after the build has settled, **run the binary once
before you use it for anything**, and quote its sha256 beside your numbers. The
same lane earlier got 15 phantom `E0560` errors in an untouched file by rebasing
a worktree while clippy was reading it. An artifact taken from a tree or a
directory that is still moving presents as a defect somewhere else entirely.

### Commit before you mutate

Non-vacuity checks work by breaking the fix and confirming the assertion
reddens, so the tree spends time in a deliberately wrong state. Two ways that
has destroyed work here:

- `git checkout -- <path>` reverts to **HEAD**, not to your pre-mutation state.
  A lane lost uncommitted fixes mid-sweep this way. Commit the fix first, then
  mutate, then restore from the commit.
- `git stash` is **repo-global, not per-worktree**. Two lanes stashing
  concurrently interleave, and `stash@{0}` is not necessarily yours. Drop by
  matching the stash message, or avoid stash entirely in favour of a commit.

### A commit message is not evidence

Re-run the suite on the tree you are about to merge, not on the tree the lane
measured. Two claims failed to reproduce on the same day:

- A commit message stated that `alpine_runtime_contracts` passed. Merged with
  tip, it **failed at exactly the assertion the message said it fixed** — a
  `poll` timeout fell through into a blocking `accept()`, so the bounded step
  that the whole design rested on had never been bounded.
- A `comm` fix was reported complete and was red on its own fixture: the
  zero-length write was short-circuited at *two* layers and the lane had found
  one.

Neither lane was careless — both were reporting a state that was true when they
measured it. Evidence ages: a rebase, a sibling merge, or a second defect behind
the first is enough. So verification is a separate job from implementation, and
the verifier re-derives rather than inherits — including non-vacuity, which is
cheap to redo and is the check most likely to have gone stale.

A commit may be called stable or buildable only after verification from that exact
committed tree. A passing build in a dirty shared worktree is not evidence for
`HEAD`: uncommitted companion schema, match, generated, test, or composition edits
may be supplying the successful build. Before handing a revision to another lane
or starting an authoritative corpus run, verify it in a clean detached worktree
or equivalent clean checkout and record the tested commit. Do not continue shape-
changing edits until the dependent verification has captured a coherent commit.

### Clippy and rustfmt only work through the pinned shell

`cargo clippy` invoked directly fails on `hl-native`'s build script with
`error[E0514]: found crate cc compiled by an incompatible version of rustc`. The
shared `target/` was populated by the flake toolchain, and a host-resolved
`cargo` is a different rustc reading the same directory. The same error appears
whenever `cargo`/`rustc` come from a distribution package and `clippy-driver`
comes from Nix, and in both spellings **the two report the same version string** —
what differs is how the builds hash crate metadata, not the version, which is why
this does not look like a toolchain mismatch. This is not a defect in anyone's
diff and `cargo clean` is the wrong response — it would discard tens of
gigabytes other lanes are using.

Enter the pinned shell with `nix develop` before running `cargo clippy` or
`cargo fmt`. Two lanes have now reported the E0514 as a mysterious failure of
their own change.

### Re-read this file before you quote it

A lane reported that this file still claimed Docker was reachable. The claim was
already corrected, **in that lane's own worktree** -- it was not behind. It had
read the file once, two hours earlier, and quoted the memory.

This file takes roughly **thirty commits a day**. A section you read an hour ago
may have been rewritten since, and the version in your head is the one you will
argue from. **Re-read the paragraph before you cite it**, and say when you read
it if the point turns on what the file says.

Checking `git log -- AGENTS.md` is the weaker habit and would not have caught
this, because the tree was current -- only the reader was stale.

The finding underneath was still real and independently arrived at, which is the
usual shape: the observation is right and the "the docs say otherwise" half has
aged out. Report the observation; verify the citation.

### Docker is NOT reachable on this host

**Corrected 2026-08-21.** This section used to say `sudo -n docker` works and
Docker 29.1.3 is running. That was true of the macOS machine the project was
developed on until 2026-08-20. On `naa0245`, the x86_64 Linux box, **Docker is
not installed at all**: no `docker` binary, no `dockerd` binary, no socket, and
`systemctl is-active docker` answers `inactive`. Verify before you rely on it
rather than trusting either version of this paragraph.

An end-to-end lane found this after being sent to it by these instructions. Any
lane told to measure the real daemon as an oracle **cannot**, and must say so
instead of quietly substituting the documentation.

The findings below were measured against the real Docker on the macOS host and
remain valid as observations of Docker's behaviour. They are **history, not
something you can reproduce here.**

That matters because Docker's documentation is thinner than its behavior. Probing
it directly produced findings no spec would have given: `"TERM "` with trailing
whitespace is rejected, `SIGRTMIN+16` is refused although signal 50 is valid
under the name `RTMAX-14`, `09` and `+9` parse but `0x9` does not, and the daemon
**unpauses before delivering every signal**, not only `SIGCONT`.

Measure the real daemon before writing a conformance assertion. State plainly if
you could not.

## Checking whether the box is busy

### Name-based checks, and how they read zero on a busy box

Use `pgrep -cx testing`, which matches the exact process name. Every
pattern-matching form is wrong in one direction or the other:

- `pgrep -cf "target/release/testing"` is **structurally blind**. Lanes set
  `CARGO_TARGET_DIR` under `/var/tmp`, so their binaries live at
  `/var/tmp/<lane>/release/testing` and never match. A measured instance reported
  2 against a true 11. A lane invoking `./target/release/testing` relatively from
  a worktree defeats it too.
- `pgrep -f "testing runtime"` and `pgrep -af "/release/testing"` **over-report**:
  the querying shell's own command line contains the pattern, so they count
  themselves. Measured 2 against a true 1.

Use `pgrep -ax testing` when you need the rows as well as the count.

### The box lock: exclusive to measure, shared to build

**`exec 8>file` truncates the file and rewrites its mtime, so a probe that uses
it destroys the evidence it is probing.** A lane checking whether the box-wanted
marker was held wrote `(exec 8>/var/tmp/husklet-box.wanted; flock -n -x 8)`.
That redirection is `O_WRONLY|O_CREAT|O_TRUNC`: it **creates the marker if it is
absent** and **moves its mtime to now** every time you look. So "does the marker
exist" can never fail after the first probe, and "is its mtime recent" reads the
timestamp of your own check. Measured:

```
before probe:      mtime=12:04:06
after  8>  probe:  mtime=12:04:46   <-- the probe moved it
after  8<> probe:  mtime=12:04:46   <-- unchanged
```

**Use `8<>`** -- `O_RDWR|O_CREAT`, no truncate. `flock` works on it regardless of
access mode. And read the mtime **before** taking the lock, not after. Treat the
announce as held if *either* a lock is held *or* the file exists with a recent
mtime; one test alone can be defeated by a stale file or by a holder that keeps
no lock.

This is `pgrep -f` matching its own argv, one level down: a check that runs,
returns a confident answer, and has already invalidated its own input. When a
probe writes anything at all, ask what it wrote over.

**Do not edit a shell script whose interpreter is stopped inside it.** Bash reads
a script incrementally by byte offset, so editing a running (or `SIGSTOP`ped)
script changes what it reads next and it resumes mid-garbage. If a parked runner
needs a fix, park the fix too: land it after the run completes, or write a new
file and have the resumer exec that.



**Sampling cannot hold a window; take the lock.** A 120-second all-clear says
nothing about minute three, and a lane lost a measurement to a sibling's gate
that started after its window opened. Widening what the sample sees does not
change that. So a measuring lane takes an exclusive lock for the *duration* of
its run, and anything that loads the box takes it shared:

    # measuring — exclusive, held for the whole run
    flock /var/tmp/husklet-box.lock -c './timing.sh'
    # building or gating — shared, waits for a measurement to finish
    flock -s /var/tmp/husklet-box.lock -c 'cargo test --workspace --lib --bins'

`flock` blocks until it can acquire, so a gate started mid-measurement waits
instead of contending, and a measurement started mid-gate waits instead of
reading a poisoned floor.

Most measuring runs have a cheap phase and an expensive one — controls are
contention-insensitive, candidate arms are not — and the `-c` wrapper would
hold the box exclusively through both. Take the lock around the expensive phase
with the descriptor form instead:

    exec 9>/var/tmp/husklet-box.lock   # once, NOT in a subshell or pipeline
    flock -x 9
    ... candidate arms ...
    flock -u 9

`flock` is per-descriptor, so `exec 9>` inside a `$(...)` or a pipeline locks
nothing and the run merely *appears* protected — the same shape as the
`pgrep -f` self-match, where the mechanism looks like it is working.

**A descriptor lock does not necessarily survive a process-boundary tool.**
`nix develop` closes inherited nonstandard descriptors: a lane took fd 9 in
its shell, entered `nix develop`, and the build continued after the lock had
disappeared.  Keep the holder outside that boundary instead:

    flock -s /var/tmp/husklet-box.lock -c 'nix develop -c cargo test ...'

Verified with `lslocks`: the descriptor form showed no holder during the Nix
build; the outer `flock` process remained a `FLOCK READ` holder for the whole
inner command.  After launching a new wrapper shape, inspect the live holder
once rather than assuming descriptor inheritance crosses every tool.

**The descriptor is inherited, so killing the script does not release the
lock.** Every child gets fd 9, and the lock lives until the last inheritor
dies: one lane killed its sweep script and the lock stayed held by the sweep
process and its ten workers, surviving SIGTERM and needing SIGKILL on all of
them. This is the same property that makes the lock crash-safe — release is
tied to descriptors, not to the script.

**`flock` has no fairness, so a waiting exclusive starves.** On Linux a pending
exclusive does **not** block new shared acquirers, so while builders keep
arriving faster than they drain, the measurement never gets its turn: one lane
sat on `flock -x` for minutes against 21 shared holders. This is the mirror of
the problem above — there a long exclusive job starved short ones, here the
short ones starve the long one — and the lock answers neither.

Announce intent before requesting, so builders can yield:

    exec 8>/var/tmp/husklet-box.wanted  # measuring lane, before flock -x
    flock -x 8                          # ... then take fd 9 and run

    # builder, before taking flock -s
    while ! (exec 8>/var/tmp/husklet-box.wanted; flock -n -x 8); do sleep 5; done

### Announcing intent, and what the lock cannot do

**Announce before you request, or the announcement is retroactive.** Take fd 8
*then* fd 9. A lane that queued its exclusive request first and announced
afterwards left three and a half minutes in which builders were free to keep
arriving — which is the whole window the announcement exists to close.

**Count lock holders, not processes named `testing`.** The quiet probe reads
`pgrep -cx testing` and will happily report zero while eleven holders are live,
because they are `cargo`, `clippy`, `nix` and a renamed engine. Walk
`/proc/*/fd` for the lock file instead: holder count is the occupancy signal,
and it is immune to every naming blind spot above.

**Signal intent with a second lock, not a plain file.** A `touch`/`rm` marker
has no crash-safety: if the announcing lane dies, is killed, or has its turn
reaped, the file survives and stalls **every** builder on the box
indefinitely — and unlike the lock, nothing releases it. A lane that used the
`touch` form had to arm a detached watchdog polling its own pid to remove it,
which is a workaround for the wrong primitive. Held on a descriptor the marker
inherits the kernel's release-on-death, exactly as the box lock does.

Advisory again, and it inherits every weakness of the lock: a builder that
skips the check is invisible. But it converts starvation into a deliberate
choice rather than an emergent one.

**The lock prevents contention; it does not schedule.** It is strictly
first-come, so it cannot express "the long restartable job yields to the short
irreplaceable one". A 3250-row sweep acquired instantly while a measuring lane
sat in a cheap phase holding nothing, and would have starved minima behind it
for hours. Express priority outside the lock: wait for sustained quiet
*before requesting it*, so the request only forms once the other lane is
genuinely finished. And prefer yielding — restartable work should release for
work that must be re-taken from scratch.

**Bound the wait.** An unbounded `flock -x` plus an unbounded quiet loop hangs
forever, and adoption of an advisory lock is never complete. Cap it, then
proceed and **record the load you actually ran at**: a run reporting "ran at
load 9, stated" is worth more than one that blocks silently.

**A crashed lane cannot wedge the box, and the lockfile is never cleaned up by
hand.** The lock lives on the file descriptor, so the kernel drops it when the
holder dies — SIGKILL and a reaped turn included; verified behaviourally.
There is no stale-lock state and no `rm /var/tmp/husklet-box.lock` recovery
step. **Deleting the path is the one thing that does break it**: it does not
release anything, and the next lane creates a fresh inode and acquires
immediately while the real holder is still measuring.

**Do not tell a lane to idle for a window the lock already governs.** A builder
under `flock -s` blocks only for the exact interval a measurement holds
exclusive, automatically, with no coordination between them — that is the whole
advantage over a granted window. Layering "wait until I say so" on top of it
brings back the serialization cost and keeps the lock's overhead.

This is advisory in the strict sense — it converts a contention problem into a
compliance problem. A builder that forgets the lock is indistinguishable from
one that holds it, so **a granted lock is not proof the box is quiet.** Keep
the name-matched check *behind* the lock, not instead of it.

### What the name checks miss, and asking the kernel instead

**`testing` is not the only thing that loads the box, and the others are the
ones we generate constantly.** A `cargo test -p hl-engine` test binary is named
`hl_engine-<hash>`, which `-x testing` cannot match and `-cf "release/testing"`
cannot match either. Engines invoked directly (`hl-aarch64`, `hl-x86_64`) are
equally invisible. One lane held a clean 120-second window for `testing` while
a sibling's gate run sat at 99% CPU throughout it. Since every lane runs the
gate, this is the most common competing load there is.

Check for all of them, and use load average as a second condition rather than
a substitute — it lags, so it confirms a busy box quickly but cannot prove a
quiet one:

    pgrep -cx testing; pgrep -c 'hl_engine-|hl-aarch64|hl-x86_64'; pgrep -c cargo

**A renamed binary is invisible to this check.** `pgrep -cx testing` matches
the exact process name, so a lane that copies the driver to `testing-bin` or
any other name runs unseen for its entire measurement and every other lane
reads the box as free. If you copy the driver, keep the basename `testing`
(`bin/testing` is fine — `-x` matches the name, not the path).

**So ask the kernel what is burning CPU, not what anything is called.** Every
occupancy check in this section is name-based or lock-based, and both miss the
same case: a process that is neither named `testing` nor holding the lock.
Measured 2026-08-20 — an orphaned `testing-d3f9382`, a renamed driver under a
deleted target directory, ran at **399% CPU for 4.9 hours**, reparented to init
after its lane exited. `pgrep -cx testing` read **0** against it for the whole
period. Four fully-consumed cores were invisible to every check anybody ran, and
every timing taken on this box during those hours was measured against them.

    ps -eo pid,pcpu,etimes,args --sort=-pcpu | \
      awk 'NR>1 && $2>50 && $3>600 {print}'

Anything above 50% CPU for more than ten minutes that you cannot account for is
your answer, whatever it is called and whether or not it took the lock. Run it
before a measurement, not after one disappoints you.

**Record elapsed time beside every result; it catches what the load average
cannot.** A load figure averages over a 32-CPU box (16 cores, two threads each;
`nproc` is 32, and this file said "eighteen-core" until 2026-08-21) and absorbs
four busy cores without looking alarming — `6.6` was recorded during those hours
and read as ordinary. What gave the orphan away was a test that takes 12.4 seconds taking
**405**. A runtime thirty times its own baseline is unambiguous where a load
number is deniable, so a result recorded without its elapsed time cannot be
audited for contention afterwards.

### Killing your own processes and nobody else's

**`pkill -x` kills other lanes' shared tooling.** Cancelling your own build with
`pkill -x flock` matches every lane queued on the box lock, not just yours: one
lane lost nineteen minutes of queueing that way. The name you are killing is
almost never yours alone — `flock`, `cargo`, `rustc`, `sleep` are all shared.
Kill your own process group or a PID you recorded when you started it.

**`pkill -f` kills the caller.** The killer's own argv contains the pattern, so
`pkill -f "bash timing.sh"` matches the cutover script running it and takes
both down — before it can start the replacement. A lane did this one message
after writing the `pgrep -f` self-match warning into this file. Resolve the PID
first and kill that, or match the process name with `pkill -x`. It is the same
hazard as `pgrep -f` with the opposite consequence: there the loop never
clears, here it fires on itself.

**Stop writing pattern kills. Use this instead.** The two warnings above have
been read and then violated **five times in one day** -- by five different
lanes, each of which had read this file. Damage so far: one lane's own shell
killed twice, a process belonging to a different lane killed by an `argv[0]`
match, and a sibling's `cargo check --workspace --all-targets` killed mid-run.
A prohibition that gets violated by everyone who reads it is a bad
prohibition, so here is the recipe. Copy it.

```sh
# 1. Collect candidate pids WITHOUT matching your own command line.
#    /proc/<pid>/cmdline is NUL-separated; your own shell is excluded by pid.
me=$$
for p in /proc/[0-9]*; do
  pid=${p#/proc/}
  [ "$pid" = "$me" ] && continue
  tr '\0' ' ' < "$p/cmdline" 2>/dev/null | grep -q 'YOUR-UNIQUE-TOKEN' && echo "$pid"
done > /tmp/victims.$$

# 2. LOOK at them before killing anything.
while read -r pid; do
  printf '%s\t%s\n' "$pid" "$(tr '\0' ' ' < /proc/$pid/cmdline)"
done < /tmp/victims.$$

# 3. Kill the explicit list, never the pattern.
xargs -r kill -TERM < /tmp/victims.$$
```

Better still, **do not search at all**: start anything long under `setsid` and
record the pid or process-group id when you start it, then `kill -TERM -$pgid`.
A process group you created cannot contain anyone else's work.

Three rules that survive every variation of this mistake:

- **`YOUR-UNIQUE-TOKEN` must be unique to you** -- your worktree path or a
  string you invented. `cargo`, `rustc`, `flock`, `sleep`, `bash`, and any
  binary name are shared with fifteen other lanes.
- **Print the list before you kill it.** Every incident today would have been
  caught by looking. It costs one command.
- **A path in your own argv is still a match.** A lane's `perf record` was
  matched by a sibling purely because the fixture path appeared on its command
  line. Reading the full cmdline in step 2 is what catches this.

**The hazard is not local to this box.** A lane matched four `cargo test`
processes on the macOS host, killed all of them to unblock its own, and only one
was its own. Resolve a PID and kill that PID.

### Long jobs, and the arms whose verdicts go missing

**Long measurements must outlive the turn that starts them.** Background jobs
are reaped when a turn pauses: one lane lost a nine-minute arm at the eight
minute mark with no results file ever written. Start anything longer than a
turn under `setsid nohup` so it survives, and have it write results
incrementally rather than only at the end.

**`setsid nohup` survives your turn, not your siblings.** It solves exactly one
problem -- the harness reaping background jobs when a turn pauses -- and none of
the others. A sibling's `pkill -x cargo` matches by name across every session on
the box, so a gate started correctly under its own session still dies. One lane
lost a twelve-arm gate this way at the eleventh minute of `check --workspace`.
Detaching is necessary and not sufficient: the durable part is that the runner
writes an `RC_<arm>=<rc>` line per arm as it finishes, so a kill costs you the
arms you had not reached rather than all of them.

**An arm with no `RC_` line has no verdict.** It is not a pass, it is not a
fail, and it is not necessarily still running. Two states look identical in a
directory of logs and must never be conflated: a log that ends with a
diagnosis **failed**, and a log that ends abruptly mid-compilation with no error
and no summary **was killed**. A lane read a stalled log directory as a live gate
and waited on a process that had been dead for minutes. Check the runner's pid is
alive before you attribute silence to slowness, and re-run any arm whose verdict
line is missing rather than inferring one.

### A single sample is not a window

**A single zero does not mean the box is free.** A measuring lane runs a
*series* of invocations with brief gaps between them, so a point-in-time
`pgrep` reads zero in every gap while the job is very much alive. Load average
cannot rescue this either: all three figures read ~0.8 while a run was sixteen
seconds underway, because they are decaying averages of a 32-CPU box.
Require **quiet for a sustained interval** — poll every few seconds and only
declare free after ~120 consecutive seconds at zero. A manager granting a
window on one sample will hand it out mid-series, and the lane that accepts it
adds load to someone's minima. `pgrep -ax testing` shows the row's start time,
which is how you tell "just finished" from "sixteen seconds in".

**Do not poll for your own long job — capture its exit code at the call site.**
`cargo clippy; echo EXIT=$?` at the invocation site is the whole technique. A
waiter built on `pgrep -f "cargo clippy"` matches its own command line and blocks
forever on itself: one lane reported lint as still running for nine minutes after
it had finished, then read a `sleep && tail` wrapper's exit code as lint's verdict and
reported a green it had not observed. `-cx` cannot rescue this, because a Cargo
subcommand has no distinct process name. The self-match hazard is generic to
`pgrep -f`, not specific to `testing`.

Timings taken without this check are not evidence. Counter ratios, code-size
deltas and categorical pass/timeout results survive contention; minima do not.

## Know which tree you are standing in

`cd` persists across shell calls. A single `cd` into a worktree silently
relocates every later `git`, `grep` and `cargo` until something changes it back,
and the output looks identical either way. This has produced a merge and a push
against the wrong tree, and a "this symbol no longer exists" conclusion drawn
from a lane's worktree and nearly recorded as a fact about the shared branch.

Prefer `git -C <path>` and absolute paths over `cd`. When a finding depends on
which tree it came from — a grep that found nothing, a test that passed, a file
that is missing — re-run it from the shared tree before recording it, and say
which tree the evidence came from.

CodeGraph resolves against the tree its index was built in, not your current
directory, so a shell that has wandered into a worktree gets structure from
somewhere else. Worktrees carry their own `.codegraph/`; if yours does not, run
`codegraph init -i` there rather than trusting the parent's index.

The same applies to **which commit**, and it has cost two lanes a detour on the
same day. A lane that cuts a worktree, works for an hour, and then compares its
lint or test findings against `main` is comparing against a tree that has moved —
one lane concluded a `loader.rs` line "does not exist upstream" when `main` had
advanced fifteen commits underneath it. Diff findings against **your parent
commit**, not against `main`.

`cargo clippy ... -- -D warnings` makes this sharper, because it denies lints the
crate's configured set only warns about, so it surfaces pre-existing findings as
though they were yours. That does not make it the wrong command — it is
`flake.nix:436`, the repository's own verification gate, and without the flag
clippy exits 0 no matter what is wrong because `workspace.lints` sets everything
to `warn`. But attribute its output against your parent before claiming or
disclaiming a finding.

## Uncommitted work does not exist

A lane spent a day diagnosing why one nix check was red, found it was two
independent layers deep, wrote the fix with the comments that make the next
reader understand it — and left the whole thing as an unstaged modification in
the shared checkout. `git log --all -S` found it on no branch anywhere. Any
lane's `git checkout`, any `git pull` touching the same file, would have thrown
away the day and left no trace that it had ever existed.

Commit before you report, and commit before you stop. A finding you can only
show as a working-tree diff is one command away from never having happened.

This is also why `git stash` is forbidden here: the stash is the same hazard with
a friendlier name. Commit on your own branch instead — a commit you later discard
costs nothing, and it is visible to everyone until you do.

If you must leave the shared tree dirty for a moment, say so in your report with
the path, so whoever reads it can rescue the work instead of stepping on it. And
if you find someone else's uncommitted work in the shared tree, preserve it
before you touch anything: `git diff -- <path> > somewhere-outside-the-repo` and
name it, then find out whose it is. Do not clean the tree first and ask after.

## The x86 arm of the scheduler lags the arm64 arm

**The symbols below are gone.** `scheduler/native.rs`, `native_aarch64`,
`native_x86` and `mark_productive` exist nowhere in the tree as of 2026-08-21;
only `testing`'s output parsers still name `hl-native-entry:`. This records the
deleted Rust executor, exactly as the "Historical: why the deleted Rust executor
translated less on arm64" subsection does, and was simply never labelled. It is
kept because the rule at the end survives its example, and because these counter
readings may be the only surviving trace of what that arm did. Do not go looking
for the code.

`native_aarch64` and `native_x86` are maintained in parallel by hand and the x86
side is repeatedly the one missing a piece. Two independent lanes found this on
the same day, both in `scheduler/native.rs`:

- `native_x86` never called `mark_productive`, so the entry-productivity set was
  permanently empty on amd64 and every suppressed entry latched forever — an
  ISA-wide regression invisible to any arm64 benchmark.
- `native_x86` never bumped `entries`, `declined_suppressed`, `declined_cold` or
  `declined_executable`, so `hl-native-entry:` printed all zeros on every amd64
  run and dumped the whole probe population into `declined_other`. Every amd64
  admission reading ever taken from that line was meaningless.

Neither would fail a test. Both would ship green, because the gates and the
benchmarks lanes reach for are arm64.

So: when you touch either arm, **enumerate what the other one does** and say
which of the two you checked. Confirm by enumerating the call sites or match
arms, not by reading the surrounding prose — one of the above was found only
because a lane listed which `NativeExit` variants reach a call and compared the
two functions side by side.

## A `#[cfg]` and its consumers must be the same width

Four instances in two days, all in the same shape: a `#[cfg]` on a module, an
item, or a `use`, disagreeing with the `#[cfg]` on the code that names it. None
was visible to any gate that runs on Linux or macOS, because the configuration
that would have said so is the one nobody builds.

- `mod terminal;` in `hl-engine/src/runtime/api.rs` was ungated while the
  re-export and every consumer in `execution.rs` carried `#[cfg(unix)]`. A
  Windows build entered a POSIX pty pump and produced **73 errors** inside it.
- `mod test_hook;` in `hl-native/src/lib.rs` was gated on the feature alone
  while the test target that calls into it was gated
  `any(feature = "native-test-hooks", windows)`. The mingw smoke builds without
  the feature, so on Windows the caller compiled and the callee did not exist.
- `execution.rs::an_unfinalized_generation_is_a_permanent_recovery_refusal` was
  an **ungated `#[test]` naming two `#[cfg(unix)]` items** -- `CheckpointControl`
  in its own file and `super::super::checkpoint` in `api.rs`. Every other item in
  that `mod tests` which names either already carried the gate; this one did not.
- `composition_test.rs::rejects_terminal_before_native_construction` is a
  `#[cfg(not(unix))]` test that built `RuntimeServices` with an `activation` and
  an `executable_authority` field. **The struct has had neither for some time.**
  Nothing rejected it because nothing compiles that arm.

The first two are a consumer wider than what it names; the third is the same
thing one layer up, in a test; the fourth is the opposite failure -- an arm
exclusive to a configuration nobody builds, which does not break, it **rots**.
Both directions have the same cause and the same cure.

**Fix by matching, and the default direction is to widen the narrow side.**
A module gated more narrowly than its consumers is usually just missing a
declaration -- widen it. Narrowing the consumer is right only when the consumer's
*subject* is the gated mechanism: a test of a `#[cfg(unix)] fn` that writes a
Unix mode belongs under `#[cfg(unix)]`, and so does a test whose name claims it
crossed a real Unix socket. Ask what the consumer is *for*. If the answer names
the platform, gate it and write the reason at the gate; if the answer does not,
the gate is in the wrong place.

**Never widen by deleting.** Removing the method, the assertion, or the field
that will not compile makes the arm green and the product smaller. Where a
mechanism genuinely has no Windows equivalent, keep the entry point and refuse in
it, with the reason written at the point of refusal -- the way
`sock_identity_directory` returns `NULL` there, and the way `storybook::host` now
answers `Fault::Socket` naming the missing socket pair rather than disappearing.

**And do not widen by substituting.** Some Unix spellings are incidental and some
are the subject. `UnixStream::pair()` used only to obtain two connected endpoints
is incidental, and a loopback `TcpStream` pair is the same three properties, so
widening keeps the test running on all four hosts -- which beats a `#![cfg(unix)]`
integration target that compiles to zero tests and reports `ok`. The same call in
`frames_cross_a_real_unix_socket_in_order` is the subject, and substituting there
would have left the test's own name a claim about something it no longer did.

**The only way to find these is to build the other configuration.** Not a grep:
`cargo check --workspace --all-targets --target x86_64-pc-windows-gnu` is what
found all four, and the CI mingw job runs `cargo build -p hl-native -p hl-engine
-p engine`, which compiles no test target and only three crates. When a
configuration cannot be built at all -- `hl-container` cannot cross to Windows
because `aws-lc-sys` stops the graph -- rewrite every `cfg(unix)`/`cfg(not(unix))`
in that crate to a cfg name that is never set, build with
`RUSTFLAGS=--check-cfg=cfg(<name>)`, and plant a type error inside the arm to
prove it was really selected. That technique is sound for cfg-graph consistency
and blind to libc and ABI differences; say which you proved.

## A worktree on one host is invisible to the other, and git will prune it

The macOS host and the Linux VM share the repository but **not** `/var/tmp`. A
worktree registered from one host at a `/var/tmp` path does not exist as far as
the other host is concerned, so any `git worktree` command run there — including
the `git worktree prune` that `git worktree add` performs implicitly — treats it
as stale and deletes its administrative directory.

Two lanes lost worktrees to this on the same day. The branch ref and the commits
survive in the shared repository, but the worktree's `HEAD`, `gitdir` and
`commondir` are gone, which surfaces as a `nix develop` failing mid-gate or as a
clippy error with no obvious cause — not as anything that mentions worktrees.

So: **do not run `git worktree` commands on one host while another host holds a
`/var/tmp` worktree**, and if a gate fails for a reason that makes no sense,
check that your worktree still has its administrative directory before believing
the error.

## A worktree under a home directory cannot be tested without privilege

`/root` is `0700`, so a test that drops privilege cannot traverse to anything
built inside it. Two lanes hit this on the same day: `rootless_publish` re-execs
its own test binary as an unprivileged account and failed with EACCES until the
binary was copied elsewhere, and a lane that wanted to certify a gate as a
non-root user could not, because doing so would have meant loosening the mode on
a directory three other lanes were working in.

Put worktrees under **`/srv/worktrees/<name>`**, which is `0755`, when the work
may need running without privilege.

**Corrected 2026-08-21: `/var/tmp` on this box is `0755` and not sticky.** This
paragraph used to say it was `1777`. `/tmp` is `1777`; `/var/tmp` is not, and
`stat -c %a /var/tmp` says so in one command. A `CARGO_TARGET_DIR` under
`/var/tmp/<lane>` still serves an unprivileged run as long as your own directory
below it is traversable, but check the mode rather than assuming the sticky bit.

That correction has a consequence nobody has chased. The root-only failure of
`stop_wait_failure_attempts_rollback_before_returning` -- named under "A single
run of a suite with a flaky member is not a count" -- has been explained on this
box, including in briefs circulated to roughly eighteen lanes, as a consequence
of `/var/tmp` being sticky and shared. **The failure is real and reproducible;
the premise of that explanation is false.** Nobody has re-derived the cause, so
treat it as unknown rather than as documented. What would settle it: run the test
as a non-root account with `CARGO_TARGET_DIR` on a `1777` path and again on a
`0755` one, and see whether the mode is load-bearing at all. Until someone has,
do not repeat the sticky-`/var/tmp` explanation -- a confidently wrong cause is
harder to dislodge than an admitted gap.

This matters beyond convenience. CI runs unprivileged, and several suites here
(`pid_namespace`, `namespace_transaction`, `unix_identity`, `seccomp_vm`) can
behave differently for an account that is not root; this box additionally sets
`kernel.apparmor_restrict_unprivileged_userns=1`. A gate certified only as root
is evidence about root, and should say so.

## Running on the other host is one wrapper away

**On the x86_64 Linux box this wrapper does not exist** -- `which mac` finds
nothing and no macOS host is reachable from it, so the paragraph below describes
the macOS/Linux pair as it was and not what you can run today. For ARM64 work
here, read "The ARM64 JIT is compiled out on this box" instead.

`mac bash -lc '...'` runs a command on the macOS host. A lane investigating a
defect that only exists on Darwin reported "I could not run on macOS" and handed
over a source-reasoned hypothesis it had never executed; another lane ran the
same investigation with the wrapper and settled it. If a defect is host-specific,
an investigation on the other host cannot conclude it -- reach for the wrapper
before declaring the measurement impossible.

Two asymmetries to carry with it. `/Users/x/dd` is shared at the same path on
both hosts; `/var/tmp` is not, and each host's copy is its own. And macOS has no
`flock(1)`, so the fd 8/9 box lock is a Linux-only protocol -- on macOS, record
the load average instead and say so.

`/Users/x/.local/bin/timeout`, the macOS GNU-timeout shim, hangs any `$(...)`
command substitution: its background watchdog `sleep` holds the pipe open, so
the substitution waits out the full duration. It cost one lane a ten-minute
sweep and left three orphaned `sleep` processes behind.

## The ARM64 JIT is compiled out on this box; two ways to get a host that runs it

The translator emits ARM64 machine code, and `src/native/host/cpu.h` gates every
part of it on `HL_HOST_CPU_AARCH64`. On this x86_64 Linux box that macro is never
defined, so `engine/target/aarch64.c:240` takes the other fork — "an AArch64 host
takes the same-ISA transliterating JIT below; any other takes interp.c" — and
`dispatch.c:177` replaces `run_block` with an `abort()` stub. Both guests still
run, correctly and to completion, entirely interpreted. Measured, not inferred:
the 18 `x86_64/lower/*.c` lowering units are compiled **0 times** in an x86_64
build and 18 times in an aarch64 one, and the engine `.so` carries **0** `e_*`
emitter bodies here against **80** there.

So on this host the ~120 emitters, the `run_block`/`block_return` trampolines,
W^X publication, chaining, IBTC, the provenance map and guard faults have no test
that can fail. **Running an aarch64 *guest* here does not touch any of them** —
the engine interprets it.

**KVM cannot close this.** KVM virtualizes, it does not translate, so it only ever
accelerates a same-architecture guest. This is not a runtime refusal you can argue
with: `qemu-system-aarch64 -accel help` on this box prints exactly one line,
`tcg`, while `qemu-system-x86_64 -accel help` prints `tcg mshv nitro kvm`. The
loaded module is `kvm_amd`. Any claim of KVM-accelerated aarch64 here is false.

What remains is emulation, in two forms, and they answer different questions.

### Fast: cross-compile here, run under `qemu-user`

The flake devshell already exports `CC_aarch64_unknown_linux_gnu`,
`CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER` and `AARCH64_LINUX_CC`, and on an
x86_64 Linux host it also carries `qemu-user`. So an aarch64 build of the engine —
JIT included — is one `--target` away and runs in place:

```text
nix develop -c cargo test -p hl-native --features native-test-hooks \
  --target aarch64-unknown-linux-gnu --test reserved_register --no-run
nix develop -c qemu-aarch64 target/aarch64-unknown-linux-gnu/debug/deps/reserved_register-*
```

The cross build takes about as long as the native one (62 s against 56 s for the
same target set, 32 threads) because the compile is native; only the run is
emulated. Use this for anything that is a property of the *emitted code* — the
emitters, the encoders, the scans — and reach for it before declaring an ARM64
question unanswerable on this box.

It reaches further than the emitters. The whole engine cross-builds and both
guests run to completion through it here -- `qemu-aarch64 hl-aarch64` and
`qemu-aarch64 hl-x86_64` on the same static fixtures the x86 engine runs -- so
translation, publication and dispatch survive being themselves translated.

Its limit is that `qemu-aarch64` is itself a translator. Guest self-modifying code,
`ic ivau`, the RWX/RX alias, real memory ordering and signal delivery into
translated frames are all reinterpreted by QEMU, so a JIT-versus-host interaction
that passes here has been tested against QEMU's model of ARM64, not against ARM64.

### Complete: the persistent aarch64 VM under TCG

`/srv/vm/aarch64` holds an Ubuntu 24.04 arm64 cloud image running a real aarch64
kernel and userland under `qemu-system-aarch64` with TCG, 8 vCPU, 32 GiB RAM and
a 250 GiB disk. Nix, flakes and the repository's own `nix develop` work in it.

```text
/srv/vm/aarch64/run.sh              # start (idempotent; refuses a second instance)
/srv/vm/aarch64/vmssh               # interactive shell in the VM
/srv/vm/aarch64/vmssh 'uname -m'    # or one command
/srv/vm/aarch64/stop.sh             # graceful poweroff, then TERM/KILL on the recorded pid
```

`vmssh` is plain `ssh` to `127.0.0.1:2222` with the key at `/srv/vm/aarch64/id_vm`,
so `scp -i /srv/vm/aarch64/id_vm -P 2222 root@127.0.0.1:...` moves files. The
serial console is `/srv/vm/aarch64/serial.log` and the QEMU pid is in
`/srv/vm/aarch64/vm.pid` — the VM is the only thing on this box you should ever
kill by that pid, never by pattern.

Getting the tree in is a tar, not a clone: `/var` and `/srv` are not shared with
the guest, and a `git worktree` registered on one side is invisible to the other
exactly as the macOS/Linux pair used to be.

```text
git archive --format=tar --prefix=husklet/ HEAD -o /var/tmp/husklet-src.tar
scp -i /srv/vm/aarch64/id_vm -P 2222 /var/tmp/husklet-src.tar root@127.0.0.1:/root/
/srv/vm/aarch64/vmssh 'mkdir -p /root/src && tar -xf /root/husklet-src.tar -C /root/src'
# then, inside: git init && git add -A && git commit  -- nix develop reads tracked files
```

Boot is about a minute. `nix develop` cost 350 s the first time and is then warm;
everything substituted from `cache.nixos.org` and nothing built from source.
Set `CARGO_TARGET_DIR=/root/target` — the VM disk is local and fast; do not put a
target directory on any shared path.

Budget for the build before you start one. A cold
`cargo test -p hl-native --features native-test-hooks --test reserved_register`
takes **17m46s** in the VM against 56 s here — and the bulk of it is a *single*
`cc1`, because `hl-cc` uses `cc` without its `parallel` feature, so the engine's
~116 translation units compile one at a time and `engine/target/aarch64.c`, the
unity TU carrying the whole JIT, is minutes of it by itself. More vCPU would not
help that phase; this is why the VM is capped at 8 and should stay there.

### The tripwire: 0 means the JIT is live, 4 means you have proved nothing

`hl_aarch64_reserved_register_test` and `hl_x86_64_reserved_register_test` emit real
code and scan it, so they answer only where the emitters exist. Off an aarch64 host
they return the sentinel **4**, "not applicable"; on one they must return **0**.
`cargo test -p hl-native --features native-test-hooks --test reserved_register`
asserts whichever of the two its own `target_arch` implies, so **the fixture is
green on x86_64 while testing nothing**. If you are certifying JIT work, read the
number, not the pass:

```text
/srv/vm/aarch64/vmssh 'cd /root/src/husklet && nix develop . --command \
  cargo test -p hl-native --features native-test-hooks --test reserved_register'
```

A green run on this x86 box is a green run of the sentinel. To read the numbers
rather than the assertion, call the two exports directly — the engine `.so`
exports both under `native-test-hooks`:

```text
cc -o verdict verdict.c -L"$out" -Wl,-rpath,"$out" -lhl_native_engine
```

Measured on the VM at 6fde34d36: `hl_aarch64_reserved_register_test = 0` and
`hl_x86_64_reserved_register_test = 0`, against `4` and `4` from the x86_64
engine `.so` built from the same tree.

### Telling translated execution from interpretation, at runtime

The engine takes no `--engine-option`, `HL_C_DIAGNOSTICS` is a launch option with
no environment importer, and `hl-native-entry:` has no producer -- so none of the
usual counters are reachable from `hl-aarch64`/`hl-x86_64` on the command line.
The code cache is visible in `/proc` instead, and it is unambiguous. The guest
runs in a **child** of the worker, so look there:

```text
./target/release/hl-aarch64 --rootfs "$rootfs" spin & P=$!
sleep 4; C=$(cat /proc/$P/task/*/children | tr ' ' '\n' | grep -E '^[0-9]+$' | head -1)
grep hl-code /proc/$C/maps
```

On the aarch64 host that prints the W^X pair -- two 64 MiB `/memfd:hl-code`
mappings, one `rw-s` and one `r-xs`.

**Corrected 2026-08-21: this probe is not a JIT tripwire, and the paragraph that
used to stand here was wrong.** It claimed the same command prints *nothing* on
this x86_64 host because there is no JIT. It prints. A measuring lane ran it at
tip and saw **four** `/memfd:hl-code` mappings under an x86_64 guest and **two**
under an aarch64 guest, on a host with no translated execution at all. The arena
exists because the interpreter bump-allocates its block descriptors from it, so a
positive `grep` says only that the arena was mapped -- never that anything in it
was translated. The same lane walked **175,644 live block descriptors** in a
running guest and found **0** transliterated. Anyone using the presence of the
mapping as evidence of JIT activity is reading a false positive; use the
`reserved_register` sentinel above (0 live, 4 not applicable) which answers the
question that was actually being asked.

Dumping
the arena settles it completely; 256 bytes off the head of one, disassembled with
`objdump -D -b binary -m aarch64`, is the translated-block prologue --
`ldr x9,[x0,#248]` / `mov sp,x9` / `msr nzcv,x9`, then `ldp q0,q1,[x0,#384]`
through `q30,q31`, then the guest GPRs -- and no instruction in it names `x18`.

### Raise `ulimit -n` before running the engine in a fresh VM

A stock Ubuntu cloud image ships a 1024 soft `nofile`; this dev box runs
1,048,576. Under 1024 the engine fails at **create**, before the guest starts,
as `NativeCreateFailed(5)` -- `HL_STATUS_RESOURCE_LIMIT` from the `pipe()` in
`engine/lifecycle.c:783`, which is simply out of descriptors after the fd cache
binds 1024 of them. Nothing in the message points at a limit. `/srv/vm/aarch64`
now persists the higher limit through `systemd` and `limits.conf`; if you build
another aarch64 host, set it there too before concluding anything about the
engine.

### Never take a timing here

TCG is a software translator and the VM is 8 vCPU of it. Every duration measured
inside the VM, and every duration measured under `qemu-aarch64`, is a measurement
of QEMU. Numbers from either are not comparable to the Mac's figures, to this
box's x86 figures, or to each other. If a number from an emulated aarch64 host
must be reported at all, label it emulated in the same sentence.

### What neither closes

CI still has no aarch64 job that runs the JIT. The `macos-26` job builds
`checks.aarch64-darwin.host-darwin-aarch64-native` and runs one ABI test; the job
that runs `cargo test -p hl-native --all-targets --features native-test-hooks` is
`ubuntu-24.04`, where both hooks return 4. A `runs-on: ubuntu-24.04-arm` job
running that one command is the smallest change that would make a reserved-register
regression fail a pull request.

## Counting and measurement

Counting what a suite reported and taking a number are one discipline. Both fail
silently, both fail in the direction that flatters the person measuring, and both
have voided real results here. The first six subsections are about reading what a
run reports; the rest are about producing a number somebody else can trust.

These were seven separate sections until they were gathered here, because a lane
hitting a counting problem had to guess which of the seven to open.

### State your counting convention, every time

A reference count is ambiguous between `passed` and `passed + ignored`, and the
difference is silent. Briefs circulated on this box carried `hl-native
--all-targets` as **112** (passed+ignored) and `hl-daemon --all-targets` as
**250** (passed-only) *in the same list*, so two lanes reported "discrepancies"
that were not discrepancies and one real move -- `hl-container --lib` 257 to
258 -- nearly got lost among them. Write `109p / 3i`, not `112`.

### Read the parent's result line, not the first one

Suites here re-exec probe children, and a child prints its own `test result:`
into the same stream:

```
test result: FAILED. 0 passed; 1 failed; ...; 42 filtered out   <- probe child
test result: FAILED. 34 passed; 4 failed; ...;  0 filtered out   <- the parent
```

A probe child always runs `--exact`, so it always reports a **non-zero**
`filtered out`; the parent's line is the one reading **`0 filtered out`**.
Discriminate on that mechanically rather than by position. Summing every line
inflates both passes and failures -- it is what produced the phantom "five
`checkpoint_linux` failures" that was quoted into a dozen briefs before anyone
re-ran the suite.

### Count the summary line, not the `FAILED` lines

`grep -c '\.\.\. FAILED'` is not a failure count. A test that re-execs itself has
a child that prints its own `... FAILED` line into the same stream, so the grep
reads **5** where `test result:` reads **4**. That phantom was carried in this
file and quoted into a dozen lane briefs as "five pre-existing failures" until
someone ran the suite three times and read the summary. `test result: FAILED. 34
passed; 4 failed; 5 ignored` is the number; anything you derive by grepping the
transcript is a guess that happens to be usually right.

### A single run of a suite with a flaky member is not a count

Two suites here are known to have flaky members, and neither announces it:

- `-p hl-engine --tests` -- a stable core plus at least two intermittent
  members. `arm64_cross_process_shared_futex` measured **3/8**;
  `inherited_pipe_ofd` **1/8**, and that one flips on `origin/main` itself.
- `-p husklet --lib --features runtime` -- besides the documented root-only
  `stop_wait_failure_attempts_rollback_before_returning`, the
  `product_checkpoint_test` group has intermittent members;
  `continue_later_restores_the_primary_sleep_tree_across_repeated_cycles`
  measured **1/3 on `main`**.

At 3/8, a single run lands anywhere from zero to two failures and a green run
proves nothing. **Three runs a side is the floor**, and say how many you took.

### `-p hl-engine --tests` inherits the caller's stdin

The checkpoint tests use `StandardStreams::default()`, which hands **the caller's
own stdin** straight to the guest. An agent shell's stdin is typically a socket
(`/proc/self/fd/0 -> socket:[...]`), and the engine refuses it outright:

```
typed guest fd 0 is a socket -- socket restore is not yet supported
```

A lane sampling a flaky test got **8/8 failures on both base and tip**, read it
as its fix having no effect, and nearly reported that. Re-run with `< /dev/null`
the same test passes and the real signal appears -- 3/8 on base, 0/8 on tip.

The general fact is worse than the workaround: **every `-p hl-engine --tests`
result in this repository is conditional on what stdin the caller happened to
have.** Two lanes comparing results from differently-invoked shells are not
comparing the same thing, and the failure is total rather than marginal, so it
masquerades as "my change broke everything" or "my change fixed nothing". Redirect
stdin, and say that you did.

### Vary the environment against the test binary, not the `cargo` wrapper

A lane probing whether a test survives without a CA store got `ok` from `nix
develop -c env ... cargo test`, and `0 passed; 1 failed` -- four times out of
four, in two working directories -- from the same test's binary invoked
directly. Two spellings of one command disagreed and only one was true. A `cargo
test` wrapper stands between you and the process you believe you are
configuring: it re-execs, it can re-resolve features, and it does not promise to
hand your environment to the test unchanged. The direct run is the trustworthy
one, and the lane caught this only because it re-ran a result it found
surprising rather than reporting the first answer.

### A measurement names its host, or it names nothing

The engine runs on two hosts and they are not interchangeable. The same guest
binary doing the same pure-CPU work measured **58x slower under the engine on
the macOS host than under the same engine on the Linux VM** — on the same
physical Mac, so the silicon is identical. `apt-get update` is 7.5 s on the
Linux host and ~119 s on the macOS host. The penalty is code-shape specific:
branchy, pointer-chasing, memory-touching code explodes, while straight-line
NEON reads 3.4x and a large file write reads 2.0x.

This went unnoticed for a long time because the head-to-head that exposed it was
taken on macOS while essentially every attribution in the perf record — per-exec
cost, the fd-scan work, `gencaches`, the floor benchmark — was taken on the Linux
VM. Those numbers were not wrong; they described a host on which the dominant
user-facing defect is absent.

So: **state the host with every number you report**, and treat a result from one
host as silent about the other. When a user-facing complaint is about the macOS
app, a Linux measurement cannot confirm or refute it. Reach for the Linux VM when
you want a fast iteration loop, and re-confirm on macOS before you believe a
ratio describes what the user feels.

### One guest is not enough for anything keyed on a guest address

With the native write-reservation gate off entirely — same engine binary, same
options, same source — base malloc measured 1,008,823 us on the sqlite guest
and 7,031,876 us on the sqlite-free one, reproduced by a second lane at 7.32x
with every other phase between 0.98 and 1.02.

The cause is not the guest binary as such. `allow_direct` is computed per
admission, and whether an entry pc qualifies for direct authority is a property
of the guest's code layout. When admissions alternate, `memory_mode` alternates
with them, and because `hl_native_cache_epoch_matches` folds `memory_mode` into
a **cache-wide** identity, every alternation discards the whole translation
cache: 1,642 epoch and 1,652 direct resets on the slow guest against 38 and 37
on the fast one, all with `mapping`, `instr` and `identity` unchanged. Removing
the flip takes that phase from 441,906 us to 61,710 us.

So a guest can put the engine into a pathological state that has nothing to do
with the phase's own work, and running the phase alone hides it completely —
in isolation both guests measure ~960,000 us. Measure on at least two guests,
run the full sequence rather than one phase, and report **every** phase: a
withdrawn table listed six and omitted a 1.37 string regression in its own data.

### Balance the arm order, or measure a 4% lie

Running base first and candidate second in every round puts a uniform **+4% on
the candidate** on this box. It was caught because the inflation appeared on
`compute`, `branch`, `intdiv` and `atomics` — phases the change under test could
not touch. Alternating (base/cand then cand/base) collapsed those four to 1.003,
0.998, 0.997 and 1.000.

Interleaving alone is not enough; the *order within each pair* must alternate.
A fixed order survives every other precaution — pinning, minima, per-arm `ok=`
verification — and none of them detect it.

The damage is not uniform, so it can invent or hide a specific verdict: `file`
read 1.039 under fixed order and 1.006 balanced, which is the difference between
a disqualifying regression and parity.

Include at least one phase the change provably cannot affect, and check it reads
1.000. If it does not, the harness is lying and nothing else in the table is
evidence.

### `bench --results` is a resumable ledger; never reuse a path

`bench` keys a resumable ledger on `--results`. Point two runs at the same path
and the second **replays the cached rows instead of measuring**, then prints a
clean `PASS`. There is no warning and the table looks perfect.

So give every run a unique results path. A lane reusing one across arms would
produce a plausible A/B table in which one arm was never executed.

Two related harness facts worth knowing before you build your own repeat loop:
each case already runs `repetitions: 3` and reports `min_us`/`median`/`p90` per
phase, so take `min_us` and minimise across your rounds on top of it. And the
guest is built by the harness per arm from the same `main.c`, so both arms share
a source but not necessarily a binary — see below.

### Identical source does not mean an identical binary

Two builds of **byte-identical source**, same tip, same toolchain, worktree paths
of equal length, differed by **152 bytes and a different sha256**. A candidate
build differed from base by 3,520.

That is why a base-versus-base null arm is not ceremony: it measures how much
ratio a phase can show for no reason at all. If the null arm's spread covers the
candidate's effect, the candidate is not evidence however clean the other
controls read. Phases with small absolute times are where this bites — a few
hundred microseconds of drift is percent-level on a 2.6 ms phase.

### A control that merely seems unaffected is not a control

Disable the code path in both binaries and measure that. Anything weaker is a
guess about which phases are unrelated, and the guess has already been wrong.

A suppression change was rejected on a 5.8% `syscall` regression. With native
execution disabled in **both** builds — so the changed code is unreachable and
the two must measure identically — `syscall` still read **1.057**, the worst
phase in the control, and 13 of 17 phases favoured base. Systematic, not random:
the candidate's engine is simply laid out differently, and `syscall` runs the
most engine host-side code per guest instruction, so layout shows there first.
Corrected, the algorithm's own cost was **1.012**. The change was killed for
~5% of binary layout.

`compute` and `branch` read 1.003 and 0.998 in the same balanced runs and would
have waved the change through. They looked like controls and were not — they
were merely phases the change did not reach, which says nothing about what else
differs between two binaries.

Where disabling the path is impossible, a base-versus-base null arm is the
accepted substitute and must read 1.000. A control derived from the mechanism is
better still: one lane used `compute` at 250 probes against 366,696 on `syscall`,
having first shown the cost scales with probes.

### A mechanism number and a workload number are different claims

A lane bounded a scan and a `/proc/<pid>/fdinfo` listing went from 43.1 billion
user instructions to 177 million -- **243x**. That figure was then repeated as
though the product had got 243 times faster at something. It had not. It is the
cost of the *mechanism*: what that path costs when you run it and nothing else.

What a developer feels is the *workload* number -- the same fix measured inside a
real program, where the mechanism is one component among many and is diluted by
everything around it. On this host the surrounding guest code carries roughly
1,000x of interpretation, so a 243x mechanism win can show up end-to-end as
almost nothing.

**Both numbers are true and neither substitutes for the other.** A diluted
end-to-end result is not a failure to reproduce the mechanism result; it is the
second half of the finding, and the ratio between them tells you how much of its
time a real workload actually spends on that path. Report both, and say which
each one is. A mechanism number quoted as a product improvement is the most
flattering way there is to be wrong.

### Say what you expect before you measure it

The lane that drew the distinction above also wrote down, *before building its
probe*, that it expected the end-to-end figure to come in far under 243x and
why. Stated in advance that is a prediction and the measurement can refute it.
Stated afterwards, the identical sentence is a rationalisation, and nothing in
the result can tell the two apart -- including for the person who wrote it.

This costs one sentence and it is worth taking every time a result could be
argued either way. It also protects a *surprising* result: a number you
predicted and got is evidence, where the same number produced by a lane that
would have explained any outcome is not.

#### A ratio has an operating point; report the shape

The same fix, the same arms, the same fixture, measured at two descriptor
counts:

```
N=64     21.484e9 -> 4.209e6   =  5,104x
N=8192   21.811e9 -> 0.418e9   =     52x
```

**Two orders of magnitude apart, and both are true.** A third figure, 243x, is
also true of the same change measured on a different fixture. None of them is
the answer, because a ratio is a property of the operating point, not of the
change.

The shape is the finding. Before, a 128x increase in open descriptors cost
**2% more** -- flat, because the cost tracked a fixed bound. After, it costs
**99x more** -- linear, because the bound is gone and the population walk
underneath it has emerged. **Flat becoming sloped is a change of kind**, and it
is far harder to produce by accident than a change of magnitude.

The strongest confirmation was not the size of the win at all: after the fix the
guest slope (x99.25) matches the native slope (x98.55) to **0.7%**. The engine's
cost now tracks the descriptor population the way the kernel's does. That is not
"cheaper" -- it is the right complexity class, and it is the claim worth making.

So: measure at two operating points at least, quote both, and lead with the
exponent rather than the ratio. A single ratio with no N attached invites the
reader to assume it holds everywhere, and here it would be wrong by 100x.

## You cannot measure the syscall half on this host

**An end-to-end workload measurement on this x86_64 box cannot validate a
spawn-path or syscall-path optimisation. It will read as zero.** This is
arithmetic, not pessimism, and it was established by a measurement designed to
find the opposite.

A lane fitted `host_instructions ~ D*guest_instructions + S*syscalls` across nine
completed workloads, engine-startup floor subtracted. The fit **failed**:
`R^2 = 0.064`, slope **-524,000** host instructions per guest syscall. A negative
per-syscall cost is unphysical, so the correlation is absent. Workloads do not
sort by syscall density; they sort very nearly backwards -- the densest
(`spawn300`, 258 syscalls per million guest instructions) expands **977x**, the
sparsest (`cc1`, 3.6) expands **1256x**.

**The model is not wrong. It is unidentifiable here**, and the arithmetic says by
how much. Take the aarch64 lane's measured `S = 3,135` host instructions per
guest syscall against this host's measured `D = 1,169`:

- densest workload: `3,135 x 258.2e-6` = **0.069%** of D
- sparsest workload: `3,135 x 3.6e-6` = **0.001%** of D

Between-workload scatter in expansion is 873-1504, about +-25%. So the largest
possible syscall contribution is **0.28% of the noise -- roughly 370x below it.**
The syscall term is not small on this host; it is **invisible**.

On aarch64 with the JIT live, `D` collapses 434 -> 1.29 and the same
~3,135-per-syscall term becomes the dominant cost. **The two components only
become separable once the JIT removes the first one.**

What follows for anyone optimising a syscall, descriptor or spawn path here:

- **Validate on the mechanism, with counters.** `perf stat -e instructions` over
  the path itself, with a null arm and a mechanism-derived control. That is how
  the fdinfo and eventfd work was validated, and it was the right call.
- **Or validate on aarch64 with the JIT live.** The cross-build takes ~70 s on
  this box; the persistent VM is for things needing a real kernel.
- **Do not ask for an end-to-end x86 confirmation of such a change**, and do not
  read a flat end-to-end result as evidence the change did nothing. Asking for
  that evidence is asking for a number that cannot exist.

This is the counterpart to the mechanism-versus-workload rule above: there, both
numbers are real and mean different things. Here, one of them **cannot be
measured at all** on this host, and saying so is the honest report.

### Reading a profile

High self-time and removable cost are independent properties, and this engine has
produced both failure modes:

- **Misattributed self-time.** `with_execution_memory` compiles to 4032 bytes
  because the guest-slice closure is inlined wholesale into it, so its row credits
  work done by its callees. Disassemble before believing a row; a function whose
  body should be twenty instructions and measures a thousand is reporting someone
  else's cost.
- **Real self-time that is still free to keep.** `ReservationEpochs::invalidate_at`
  is a genuine 112-byte function and its row is honestly its own, but deleting it
  along with all 5.17 billion of its atomics changed nothing measurable, because
  the `ldadd` discards its result and retires without blocking anything.

So a profile row justifies investigating a symbol. Only a mutation justifies
believing the cost can be recovered.

### A serializing instruction collects the skid of everything ahead of it

The two failure modes above are about a *symbol*. This one is about a single
instruction, and it is the reason a `perf annotate` row can be enormous and worth
nothing.

`run_guest()` makes two `seq_cst` stores per dispatcher crossing -- `cpu->irq = 0`
(`engine/dispatch.c`) and `in_translated = 1` (`translator/cache.c`) -- and both
compile to `xchg` on x86. Measured 2026-08-20 on `naa0245` with the JIT-less
x86_64 engine, `perf record -e cycles:u` over a guest fork+exec put `run_guest` at
17.0% self-time with **80.5% of its samples on the instruction after the first
`xchg`** and 13.6% on the instruction after the second: 13.6% and 2.3% of all user
cycles, in two instructions.

None of it came back. A candidate that skips the first store whenever `irq` is
already clear -- the common case, and a change the instruction counters confirm
reaches the binary, +21.5k instructions per spawn on a static x86_64 guest and
+63.6k on aarch64, exactly the load and branch it adds -- measured 1.0371 on
aarch64 in a twelve-round balanced ABBA, a **3.7% regression**. A second build of
the *same candidate source* measured 0.9868 on the same arm in the same run. The
two builds' per-round ranges do not overlap: [1.0276, 1.0432] against
[0.9825, 0.9929]. A variant with both stores weakened read 0.9908.

Two durable things:

- **`xchg` drains the store buffer, so it retires slowly and the sampled PC skids
  onto the instruction after it.** What the row measures is the queue in front of
  the barrier, not the barrier. A store nobody was waiting on can therefore carry
  a double-digit share of a profile and cost nothing.
- **One null arm is not the noise floor.** Base-versus-base read 1.0016
  [0.9912, 1.0142] in the same run and would have certified a 3.7% verdict as
  real. Layout noise is a property of the *pair* of binaries, so a candidate needs
  a second build of its own source before its ratio means anything -- the same
  reason "Identical source does not mean an identical binary" exists, one step
  further on.

The count that explains the whole thing: a spawn retires 112M user instructions in
~21,500 crossings, so a crossing is ~5,200 instructions and one locked store
cannot be a percent of it. On a host **with** the JIT a crossing is one translated
guest basic block, tens of instructions, and the same two stores are a different
question that no measurement on this box can answer.

Time the mechanism before sizing a fix for it. The native/host operand round trip
was assumed to cost about a microsecond and to dominate sqlite; measured, it is
105ns and 0.35% of the phase, so an entire direction was worth a tenth of a
percent. A count is not a cost until you have multiplied it by a measured one.

Counters are comparable within a build and not across builds. Adding
instrumentation changes inlining, which changes translation admission: two builds
of the same source reported 892,141 and 1,593,713 for the same counter. Compare a
counter only against itself in the same binary.

## A C change reaching the binary is a separate claim, and it has its own check

The engine is **dlopened, never linked**. `libhl_native_engine.so` is built by
`hl-native`'s build script into its Cargo `OUT_DIR`, and the process loads it at
runtime: on Linux the worker does not contain the engine and does not list it in
`ldd`. Three consequences, each of which has cost a lane real work:

- **A Rust binary's sha256 is unchanged by almost every C edit.** That is the
  designed behaviour, not a symptom. A lane read four byte-identical test-binary
  hashes across base, a patched tree, a reverted tree and a forced relink, and
  concluded its C never reached the build. The hashes proved nothing either way.
- **Copying the test binary does not snapshot the engine.** The same lane saved
  `bin/base`, `bin/fixed` and `bin/fixed2`; all three are one file by sha256 and
  all three resolve to the single mutable `.so` in the target directory — the one
  the *last* build wrote. Arms cannot be separated that way. To hold an arm, copy
  the `.so` too, or give each arm its own `CARGO_TARGET_DIR`.
- **The stale `.so` is real and has fired repeatedly.** A `.so` built into one
  `build/hl-native-<hash>/out` while a binary loads another — a different feature
  set, a different target directory, or a copy taken at another moment — runs the
  previous engine silently.

### What is and is not sufficient evidence

Insufficient, all three demonstrated:

- The **static archive** changing, or containing the new symbol. `libhl_c_backend_target_aarch64.a`
  grew and carried the new symbol in exactly the run that was later believed stale.
- **`strings` on the `.so`.** It answers what is on disk, not what the process mapped.
- The **Rust binary's hash**, in either direction — see above.

A **runtime `fprintf` probe** is sound only if you first prove the line you are
comparing against actually prints. The lane's decisive check was an unconditional
`fprintf` added beside the existing `[ckpt] coordinator pid=` line; the new line
did not appear, and that was read as proof of staleness. Neither line appears in
any of the twenty logs it preserved, because `ckpt_coordinate_and_exit` is not on
that test's path at all. The check compared an absent line to an absent baseline.
If you use a runtime probe, put it somewhere whose output you have already seen.

### The check to run

`cargo test -p hl-native --test build_freshness`

It recomputes a content fingerprint over every file under `src/native` and compares
it to the value Cargo baked into the running executable, and it loads the engine so
the loader's own comparison runs. They diverge exactly when the executable predates
the tree. The same value is compiled into the `.so` and exported as
`hl_c_backend_build_fingerprint`; the loader refuses a shared object whose value
disagrees and names both fingerprints, so a stale artifact is a load failure rather
than a quiet measurement of the previous engine.

Because the fingerprint is a `cargo:rustc-env`, a C edit now also invalidates the
Cargo fingerprint of every Rust artifact built against `hl-native`. Downstream
binaries are rebuilt and their hashes do change — the byte-identical reading that
started this is gone. That is a deliberate cost: a C-only edit now recompiles the
Rust crates above `hl-native` as well, which on this workspace is about two minutes
rather than forty seconds. It buys the property that a saved binary is an arm.

Incremental builds themselves are sound, in the shared tree and in a worktree with
its own `CARGO_TARGET_DIR` alike: the build script's `rerun-if-changed` covers every
file under `src/native`, and it recompiles and relinks unconditionally when it runs.
`cargo clean` is not the remedy for anything here.

### `build_freshness` red on the shared tree means the tree moved, not that Cargo failed

A lane merged into `/Users/x/dd/husklet`, ran `cargo test -p hl-native --all-targets`
twice, and failed `build_freshness` both times with the same pair of fingerprints —
then `touch build.rs` and the next run was green. It read that as
`cargo:rerun-if-changed` not firing for `src/native`. It fires. Three things were
measured that day and all three exonerate Cargo: the build script emits 649
`rerun-if-changed` lines covering every directory and every file under `src/native`
and Cargo honours all of them; the same edit-then-build cycle was reproduced on
virtiofs (`/Users`) and on VM-local btrfs (`/var/tmp`) with identical results, with
the target directory on either filesystem and no clock skew between them; and Cargo
**backdates** a build script's `output` file to the start of the invocation, precisely
so an edit made during a long script is not lost.

What actually happened is in the reflog. `src/native` changed at 12:20:42, again at
12:21:09, and again at 12:29:13 — three merges inside the lane's six-minute window.
The gate build reads the C sources when the build script starts and the gate test
reads them when the test runs, and `hl-native`'s build script compiles the whole
engine in between. Any merge landing in that gap makes the two reads disagree, which
is exactly what `build_freshness` says. The `touch build.rs` run was not a fix; it
was the first run that happened to fall inside a quiet window.

Reproduced deterministically in a detached worktree: edit a C file, start the gate,
and edit a second C file in another directory 22 seconds in — the run fails with the
same shape every time. Leave the tree alone during the build and the same two files
rebuild and the gate is green, so the mechanism is the concurrent write and nothing
else.

So: **run the gate in your own worktree, and do not merge into a checkout while a
gate is building in it.** A red `build_freshness` in a shared checkout is first
evidence that somebody wrote to `src/native` during your build; re-run it in a
worktree before believing anything about the build system. The build script now
refuses this case at the moment it happens — it recomputes the fingerprint after
linking and fails with both values if the sources moved under it — so the mixed
artifact is never handed to a lane as a silent one.

## The table is the unit of iteration and the unit of capacity

Four independent instances were found in a single day, by four lanes that were
not looking for the same thing:

| site | scans | population it should have scanned |
|---|---|---|
| `fdvis_find` | 131,072 slots on every miss | live entries |
| `proc_fdinfo_dir_open` | 65,536 fd numbers per listing | open descriptors |
| `jit_instruction_map_lookup` | 262,144 entries per guest fault | the faulting block |
| the interpreter block index | ~900 hash-scattered pages per spawn | resident blocks |

Two more share the shape without the cost: `g_fdpath[HL_NFD]` is indexed
directly and `hl_fdcache_evict_path` keys on the table rather than the open set.

The unifying statement is not "four arrays are too big". It is that **cost
tracks the bound rather than the population, and exhaustion is a cliff rather
than a slope.** Both halves bite:

- **Cost.** A `/proc/<pid>/fdinfo` listing measured **flat in N** -- 906.9 ms at
  64 open descriptors against 946.0 ms at 8,192, a ratio of **1.043**. Opening
  128x more descriptors cost 4% more, because the scan is over the fixed table.
  That flatness is the signature; look for it before assuming a cost is
  proportional to work.
- **Exhaustion.** `g_gbus` is capped at `GNA_MAX = 512`, and on overflow sets a
  flag that makes every address in the process fault **permanently** -- the guest
  dies `SIGBUS` at 512 live past-EOF mappings where the host is clean at 700. Its
  sibling `g_gna` fails *open* on the same overflow. Neither degrades; one bricks
  the guest and the other stops protecting it.

When you meet a fixed-size table here, ask both questions. **Does a miss cost
`N` or cost one?** And **what happens at `N+1` -- does it decline, or does it
change behaviour for everything?** A bound that declines the operation it cannot
record is defensible. A bound that silently changes a global answer is a defect
waiting for a busy day.

## Time-to-evidence and agent utilization

Elapsed time to authoritative compatibility evidence is the primary operational
optimization target. CPU is not a scarce resource for repository work: use every
logical CPU when a test, corpus run, compilation, or independent analysis can
benefit from it. Do not serialize work merely to keep CPU utilization low, and do
not default compatibility runs to one worker when the host can safely run more.

RAM, disk space, process-table health, and source/build ownership remain hard
constraints. Before and during wide execution, monitor available memory, swap,
free disk, output growth, and zombie or escaped descendants. Bound per-worker
captures and timeouts, preserve resumable results, and reduce concurrency only
when measured RAM, disk, thermal, or lifecycle evidence requires it. A slow run
must report whether it is limited by CPU, memory, disk, fixture setup, process
startup, locking, or guest timeouts; unexplained serialization is not acceptable.

Keep all available Codex subscriptions and agent slots productively occupied.
Managers must continuously delegate broad, independent, non-overlapping migration
domains, require direct C-oracle and Rust-source study, and replace a completed
assignment with the next highest-value compatibility gap immediately. Each Codex
manager should use its own subagent capacity fully. Coordinate shared-tree edits
and build ownership so maximum agent utilization does not create conflicting
patches or invalidate evidence. Prefer parallel read-only audits while one owner
performs a shared-tree build or authoritative run.

Keep implementation sessions short-lived and outcome-bounded. A normal lane owns
one coherent capability for at most 20 minutes; an external manager coordinating
several independent subagents has a hard 30-minute lifetime. It must then deliver
one audited commit with exact-tree evidence, or a concise source-backed blocker
report, and exit. Repeated diagnosis, fixture-by-fixture iteration, or widening
the lane after its original capability is exhausted is not progress: stop that
session and give a fresh agent the next bounded domain. Preserve unfinished work
on its branch or worktree; never manufacture a cosmetic commit merely to meet the
deadline.

Compatibility workers receive engine launch options only through the typed
`HL_COMPAT_ENGINE_OPTIONS` setting. Setting an engine option directly in the
inventory supervisor's ambient environment does not configure the guest engine and
must never be cited as evidence of a selected mode.

### There is no native/interpreter switch, and the counters that proved it are gone

This subsection used to instruct lanes to request native execution with
`HL_NATIVE_EXECUTION=1` and to confirm it from `probes`/`entries` on the
`hl-native-entry:` line. **Every part of that is now stale, and following it wastes
a lane.** Re-derived on 2026-08-18:

- `HL_NATIVE_EXECUTION` had no consumer anywhere in the repository — C or Rust. The
  authoritative engine registry is
  `src/runtime/hl-native/src/native/engine/options.c`, and it never defined any
  `HL_NATIVE_*` option. The eight dead entries have been removed from
  `src/containers/hl-engine/src/options.rs`; `retired_native_executor_options_are_not_registered`
  pins their absence.
- `hl-native-entry:`, `probes` and `entries` have **no producer** in the tree. Only
  testing-side parsers mention them. A lane that waits for those counters is waiting
  for output nothing emits, and may conclude its own build is broken.
- There is no interpreter in the production engine. `translator/` and
  `engine/target/{aarch64,x86_64}.c` are the execution path; the native-vs-interpreter
  split belonged to the deleted Rust executor, which the "Historical" subsection below
  already marks as gone. Container workloads were never interpreter numbers.
- The engine workers accept **no `--engine-option` flag at all**. `LaunchArguments`
  (`src/apps/engine/src/lib.rs:55-68`) has only `--guest-isa`, `--report-exit`,
  `--rootfs`, the executable and its arguments, and both construction sites use
  `Options::default()`. Measured: `./target/release/hl-aarch64 --engine-option
  HL_C_DIAGNOSTICS=1 /bin/ls /` exits **2 with no output**, while the same command
  without the flag runs the engine. So an unknown option does not get silently
  ignored — it kills the run before the guest starts.

The durable lesson survives its example: **confirm a mode from a counter, not from the
command you believe you ran** — but first confirm the counter still has a producer.
The instruction above outlived the mechanism it described by long enough to mislead
several lanes.

Retained-C diagnostics remain real and are requested with `HL_C_DIAGNOSTICS`, which is
the one launch effect `Execution::Native` still carries.

## Mission

Provide isolated, reproducible Linux workspaces backed by the production C
execution engine in the Cargo package `src/runtime/hl-native`, with a memory-safe
Rust control plane.
Opening a workspace enters its configured
image with a terminal, filesystem, networking, VPN, and container services.

Preserve exact Linux behavior across AArch64 and x86-64 guests and Linux, macOS,
and Windows hosts. The product composes replaceable engine, container, workspace,
and terminal capabilities; reusable crates contain no Husklet product policy.

Ordinary CLI and terminal applications must run without application-specific engine
workarounds. The final compatibility/performance gate includes container workflows,
interactive terminal workloads, and nested engine execution such as `arm -> amd -> arm`.

Production engine behavior must never branch on an application, language, runtime,
framework, executable name, build-information marker, or vendor identity.  In
particular, Go, V8, JVM, and similar guest internals are not Linux ABI
domains. When inherited C contains such a branch, preserve it as migration evidence
and identify the violated generic invariant (for example non-PIE guest-address
placement or signal semantics); repair that invariant in its authoritative
owner rather than adding runtime-specific detection in C or Rust. Guest-visible
addresses remain ELF/Linux addresses;
host storage placement is an internal mapping detail and must not leak into guest
pointers, symbols, signals, `/proc`, checkpoints, or runtime metadata.

## Production/native ownership study before every engine lane

Reading compatibility fixtures and expected output is necessary but insufficient.
Before changing a runtime domain, the lane owner must inspect the selected
implementation under `src/runtime/hl-native/src/native` and, when provenance or an
independent baseline matters, the corresponding pinned read-only implementation
in `../engine`. Record:

- the exact C and assembly files and entry functions studied;
- state ownership, identity, lifetime, locking, and teardown behavior;
- syscall ordering, partial-result, blocking, cancellation, signal, and errno
  semantics relevant to the lane;
- architecture-specific and host-specific branches;
- the explicit mapping from each observed capability to its current production
  C owner, Rust control-plane boundary, or honest remaining replacement gap.

Record this oracle audit beside the relevant compatibility or performance report
before the lane is accepted.
An agent report that cites only tests, manifests, expected output, or summaries
does not satisfy this requirement. Never edit `../engine` while performing the
audit.

### The oracle is authoritative about intent, not about the kernel

`../engine` shipped, so what it *does* is strong evidence about what guests
depend on. It is not evidence about what Linux does, and where a host
measurement and the C disagree, **the kernel wins**. An oracle comment asserting
kernel behavior is a claim to test, not a fact to port.

Two lanes found this the same day, in unrelated domains:

- `src/linux_abi/syscall/io.c:1384` states that a comm write "drop[s] one
  trailing newline" and implements it, and ignores a zero-length write. The host
  kernel does neither — only NUL terminates, and a zero-length write clears
  comm. The Rust had faithfully reproduced the wrong comment.
- `src/linux_abi/container/state.c:596` initializes the capability sets to
  `HL_CAP_DEFAULT` unconditionally and `HL_UID` never reaches them, so a C
  `--user` container reports the full container set. Linux clears
  permitted/effective across a root-to-non-root transition. The Rust is ahead of
  the oracle here, and following the C would have been a container-escape
  regression.

So: measure the host first, then read the C to learn what the guest-visible
contract is meant to be. When you override the oracle, say so and show the
measurement. A fixture that passes on the bare host kernel as well as in the
engine validates the assertion; one that only passes in the engine validates
nothing.

### Change domains, not failing cases

The integrated `hl-native` C backend is the production implementation. Compatibility
cases are acceptance evidence and prioritization signals; they are not a
substitute for understanding the complete implementation that already works.

Before fixing a corpus cluster, read the complete integrated C domain and its call
graph rather than only the function named by the first failure. Inventory every
entry point, state object, ownership edge, lock, wakeup, error path, architecture
branch, and teardown transition, then compare that inventory mechanically against
the current in-repository owners. Record a dense capability matrix with each
capability marked selected, replacement-candidate, divergent, or missing.
Implement the largest coherent missing mechanism and all of its widths, flags,
lifecycle paths, and error semantics before returning to the corpus.

Walking one executable until it exposes the next unsupported instruction or
patching one fixture-visible branch at a time is forbidden when the integrated C
tables or domain implementation can reveal the complete family in one audit.
Likewise, a narrow passing case does not prove a domain port complete. Acceptance
requires focused cohort evidence after the implementation comparison and later a
full-corpus checkpoint from the exact committed tree.

## Source layers

The source tree separates reusable foundations, engine runtime domains, native
execution, container capabilities, workspace capabilities, and the product root:

```text
src/
  packages/   transferable libraries and repository tool packages
  runtime/    engine runtime domains, including the hl-native Cargo package
  containers/ container services and the integrated hl-engine
  workspaces/ workspace, terminal, and generic GUI capabilities
  apps/husklet/ the product composition root
```

Dependencies point inward:

```text
husklet -> workspaces + containers -> runtime -> packages -> std
                              -> hl-native (Cargo-built C engine)
```

- Production libraries in `packages/` must make sense without an engine, guest,
  syscall, emulator, or container.
- `runtime/hl-native` owns the integrated C execution engine and its narrow Rust FFI boundary.
- `containers/hl-engine` owns validated plans, lifecycle supervision, and product-facing engine composition.
- `apps/husklet` selects product adapters and composes containers, workspaces, terminal, and GUI.
- No package depends on `apps/husklet`.
- Repository tools live as packages under `src/packages/`, but remain build-time
  machinery and never production dependencies. The generic `hl-design` annotation
  package is the only explicitly reviewed exception when used by production crates.

Changing a local Cargo dependency requires explaining the ownership reason and
passing the dependency linter.

### UI ownership

- `hl-gui` owns generic visual primitives, layout, validation display, accessibility,
  and toolkit adapters.
- Husklet owns screens, settings schemas, product view models, navigation, and feature
  composition.
- Generic components receive state and emit typed intent. They do not persist,
  orchestrate, or invoke services.
- Product components such as workspace pickers, image choosers, removal confirmations,
  and terminal settings stay beside the feature that owns them.
- Native toolkit types do not cross the GUI boundary.
- Add a component only for a stable concept, state contract, interaction contract,
  accessibility behavior, or cohesive reuse; keep one-off layout beside its page.

## Package placement

Ask these questions in order:

1. Is it repository-only lint, differential, fixture, or benchmark machinery
   that is forbidden as a production dependency? Put its package in `packages/`
   and keep the tool boundary explicit. Product compatibility probes and audits,
   including syscall admission audits, are owned by the `testing` application;
   they do not become runtime packages.
2. Does the code extend ordinary logging, filesystem, byte I/O, encoding, or
   another standard-library mechanism without engine vocabulary? Put it in
   `packages/`.
3. Does it implement guest execution, Linux ABI behavior, translation, or a
   host adapter for the integrated engine? Put C implementation in the owning
   capability below `runtime/hl-native/src/native`; keep the Rust surface to FFI
   bindings and safe wrappers.
4. Does it connect engine capabilities or select a concrete host adapter? Keep
   that integration inside the C engine below `runtime/hl-native/src/native`.
5. Does it validate engine configuration, expose the engine API/CLI, or
   construct the complete engine? Put it in `containers/hl-engine`.
6. Does it own product configuration, screens, commands, navigation, or cross-domain
   composition? Put it in `apps/husklet`.

Do not add catch-all packages or modules named `core`, `common`, `shared`, `types`,
`utils`, `helpers`, or `misc`. Name code by the entity, capability, algorithm, or
external mechanism it owns.

Do not create an outer directory containing one crate. The source layers are the
meaningful grouping. Engine concepts such as ISA, memory, networking, tasks,
and execution are capability-owned C subtrees inside
`src/runtime/hl-native/src/native/`; they do not become sibling Rust runtime packages.

## Domain ownership

Each native engine capability owns:

- its entities and value types;
- valid-state construction;
- lifecycle and concurrency invariants;
- domain operations and typed errors;
- consumer-owned capability traits;
- pointer-free, bounded snapshot values;
- platform adapters only when the mechanism belongs solely to that domain.

The C engine exposes one small, versioned boundary through `hl-native`; other
packages must not import native implementation headers or reproduce its models.

Cross-capability operations remain inside the integrated C engine:

| Operation | Domains joined |
|---|---|
| file-backed mapping | descriptor + VFS + memory |
| procfs | VFS + task |
| signalfd | event + task + descriptor |
| Unix pathname socket | VFS + network |
| `SCM_RIGHTS` | network + descriptor |
| fork | task + descriptor + memory + execution |
| exec | task + loader + descriptor + memory |
| provider-backed object | provider + receiving domain |
| syscall trap | execution + Linux personality |
| checkpoint | all snapshot-capable domains |

These adapters use public APIs and owned values. They never access private fields.

## Ports and adapters

A port is a narrow trait owned by the consumer that needs the capability. Add a
port only for a real platform, substitution, testing, FFI, or stable domain
boundary.

Current execution boundary:

- `hl-engine` owns validated launch plans, worker supervision, fail-closed
  C-only backend selection, and the coarse `hl-native` FFI;
- Rust loader and memory services own bounded image/projection inputs;
- selected C owns guest scheduling, Linux personality, translation, and execution;
- memory owns `Backing`; runtime adapts a pinned open-file description;
- VFS owns `VfsHost`; the app supplies the selected host adapter;
- network owns `SocketHost`; the app supplies the selected host adapter.

Never introduce a shared `host-api`, service locator, or omnibus platform trait.
Keep traits small and capability-specific.

## Native execution boundary

The production C/assembly engine lives under
`src/runtime/hl-native/src/native` and is compiled by
`src/runtime/hl-native/build.rs`. It is selected through the Rust boundary in
`src/containers/hl-engine/src/runtime/execution.rs`. It currently owns the complete guest
execution closure: translation, scheduling, Linux ABI services, signals,
filesystem and descriptor behavior, and guest lifecycle. Rust owns product
selection, launch-plan validation, worker supervision, and the public product
interfaces. Builds must not read or link `../engine`.

There is no second production engine tree. Differential or oracle material belongs
under testing, and the build must not select code from `../engine` or
`../engine_rust`. Cargo is the package-level build authority; Nix supplies and
pins its toolchain.

The following narrow boundary described the deleted Rust-executor migration and
is retained as historical design evidence, not as the current architecture:

- CPU layouts whose offsets are embedded in machine code;
- assembly entry, block-return, and trampoline code;
- W^X code-cache mutation, publication, lookup, and chaining;
- POSIX signal/ucontext and Windows VEH/CONTEXT entry;
- fault-context reconstruction;
- async-signal-safe and fork-critical repair.

The candidate boundary was not intended to own Linux syscall, filesystem,
descriptor, networking, task, loader, checkpoint, or product policy.

Cross-language operations are coarse. FFI per instruction, guest memory access,
block lookup, or chain transition is forbidden.

CPU layouts shared across the C/Rust boundary live with `hl-native`; both sides
compile size, alignment, and offset assertions. Hand-maintained duplicate layouts
are forbidden.

### Historical: why the deleted Rust executor translated less on arm64

The measurements and symbols in this subsection describe the deleted Rust
executor. They remain useful performance evidence but do not describe current
production routing or ownership.

arm64 still retires far less translated code than amd64 on short programs, but the
figures this section used to carry are stale and one of its three limiters has been
fixed. Re-measured at 4d2fe7777 in release, runner sha256 `05ad308c…`, one case per
invocation with `--jobs 1` (`testing runtime --case <id> --isa <isa> --jobs 1`); the
suite already sets `diagnostics: true`, so the counters below come from that build
alone and are not comparable with any other.

`runtime/syscalls/gettid`:

- arm64: `interp instructions=96982`, `runs=1 builds=36 hits=82 fallbacks=0`,
  `probes=39 entries=1 declined_cold=38`, `completed=258`.
- amd64: `interp instructions=107996`, `builds=86`, `completed=25558`,
  `x86_cold_builds=86`, `relocation_cold_targets=138`.

Two limiters remain, and the third is gone:

- **The first entry no longer takes direct authority; it earns it.** The old reading
  was correct for its tree: `native_slice` derived `allow_direct` from `direct_holds`
  and `direct_declined`, both empty on a process's first probe, so the first arm64 run
  took direct mode, which carries no operand resolver, and ended at ~8 instructions on
  `a64_fallback_guard_write=1`. `direct_earned` (`scheduler/pool.rs`) now also requires
  a `direct_modes` entry, which only a completed run creates, so the first run spends
  the resolver. That is what moved the 22-row signature `builds=1 hits=2 sites=1
  entries=1 completed=8` to `builds=36 … completed=258`: commit 271a6e86e, not drift.
  `scheduler.rs::direct_authority_is_earned_by_a_completed_run` pins it -- that test
  no longer exists either, checked 2026-08-21. x86 still has
  no direct authority at all — `run_x86_lease` takes no `allow_direct`, `run_x86_inner`
  always passes an operand resolver, and the x86 `RunStatistics` hardcode
  `direct: false` and `direct_guard: false`. Its sixth positional argument is
  `interrupt`, once a literal `false` and now `run.interrupt.is_set()`; it was read as
  `allow_direct` more than once, so read the signature, not the call site.
- **The run still exits on a cold branch target instead of building it.** The surviving
  arm64 gettid run ends at `a64_branch_exhaustion=1`, `a64_branch_cold_relocation=27`.
  amd64 builds through the same targets inside one lease.
- **Re-entry still never happens.** `observe(key) < 2` requires the same
  `(process, generation, version, pc)` to be probed twice, but native is probed at most
  once per 4096-instruction interpreter slice; gettid gets 39 probes over 38 slices and
  38 are `declined_cold`. amd64 escapes this only because, once in, it does not come
  back out.

So fixing the first limiter bought one resolver run per process — ~259 instructions of
one slice out of ~97,000 — and did not make a short program run translated.
`signals/folded-fault-registers` and `signals/implicit-null-pc` still fault interpreted
and do not test the mechanism their sources claim. Making them honest requires changing
the arm64 warm-up gate, which is an admission change and carries the full guard.

`process/sysinfo` is a member of that same arm64 set (arm64 `builds=37 completed=259`,
`interp instructions=16989`), not a separate weak row. The claim that its amd64 side was
complete native coverage was an artefact of a broken counter: `run_x86_slice` built no
`InterpreterTally`, so `hl-interp: instructions=` printed 0 on every amd64 run and amd64
coverage read as a flat 100%. Fixed in 1cb4a1287. sysinfo amd64 now reads
`builds=22 completed=79` against `interp instructions=23818`, so its native share is
under 1%, and gettid amd64 is 25558 native against 107996 interpreted. Never quote an
amd64 `interp instructions=0` from before 1cb4a1287.

Do not read `a64_fallback_form_memory` as a form classification. It is incremented
both by the word classifier and, unconditionally, by the guard-fault path, so it is
at least the guard-fault count by construction and can never be a minority. The
claim that it was 278,672 of 278,672 on sqlite is that identity, not evidence that
the operand-resolver memory path is the whole fallback population.

## Unsafe code

Workspace code forbids unsafe by default.

Unsafe is permitted only in reviewed modules that implement:

- platform system calls;
- the native execution ABI;
- the external C ABI;
- memory mapping and fault entry that cannot be expressed safely.

Every unsafe block states:

1. the validity, lifetime, alignment, and aliasing assumptions;
2. which owner keeps referenced storage alive;
3. why concurrent access is valid;
4. why failure cannot unwind across FFI.

No allocation, lock acquisition, logging, panic, unwinding, or Rust destructor walk
may occur in a signal, VEH, or fork-critical callback.

## Types and ownership

- Make invalid states unrepresentable with constructors, enums, and meaningful
  newtypes.
- Do not wrap primitives or collections without an invariant, identity boundary, or
  cohesive behavior.
- Borrow for observation and transfer ownership for storage.
- Clone only when the ownership model requires independent ownership.
- Use checked arithmetic where overflow is invalid and saturating arithmetic only
  where clamping is the contract.
- Guest-provided lengths, counts, offsets, command batches, and resource requests
  must be bounded before allocation or expensive host work.
- A descriptor, OFD, mapping, task, subscription, provider handle, and translated
  block each have one explicit owner and generation/lifetime model.
- Do not use process-global mutable state for engine instances.

## Errors and Linux behavior

Libraries return typed domain errors. Linux errno conversion happens at the Linux
personality boundary.

Preserve:

- exact `EAGAIN`, `EWOULDBLOCK`, `EINTR`, and partial-I/O behavior;
- shared OFD offsets and descriptor-local flags;
- epoll edge, level, oneshot, timeout, cancellation, and wakeup ordering;
- `SCM_RIGHTS` ownership;
- shared mapping visibility and protection ordering;
- futex deadlines and wakeups;
- fork/exec descriptor, signal, task, and mapping transitions.

Do not panic for guest input or recoverable host failures. No panic or unwind may
cross a C boundary.

## Concurrency and performance

- Avoid global locks across unrelated engines, processes, descriptors, mappings, or
  translated blocks.
- Do not hold table locks across host calls.
- Unrelated OFDs must not serialize.
- Define task ownership, cancellation, shutdown, and wakeup ordering.
- Backpressure blocks or rejects predictably; it never busy-spins.
- Do not log every syscall or translated instruction in normal operation.
- Do not introduce synchronous full-frame, device-wide, or whole-engine waits in a
  hot path.
- Preserve explicit bounds for caches, commands, memory, threads, handles, logs, and
  retained resources.

Every hot-path migration compares against a pinned C baseline. Nested engine
benchmarks measure compounding overhead.

### `g_threaded` is not "is anything else running"

`g_threaded` is set by the clone service and means **the guest has more than one
guest thread**. The dispatcher guards its lock, its safepoint and its generation
publication with it, which is correct for those, because each one's counterparty
is another `run_guest()`. It is the wrong guard for anything whose counterparty is
a *host* thread, and the engine creates several of those with the guest still
single-threaded:

- `hl_checkpoint_control_main` (`engine/lifecycle.c`) and `checkpoint_relay_main`
  (`linux_abi/thread/lifecycle.c`) both store `cpu->irq = 1` on every registered
  executor, from their own thread.
- `gtimer_loop` (`linux_abi/syscall/time.c`), the POSIX-timer drain thread, reaches
  `thread_target_signal_info()` for a `SIGEV_THREAD_ID` expiry, which publishes
  `tpending` and then stores `cpu->irq = 1`.
- `bound_watch_waiter` (`linux_abi/syscall/binding/watch.c`) calls
  `hl_linux_bus_transition_begin()`, which is the *quiescing* side of
  `stw_before_translated()`'s `in_translated` handshake. It is started by an
  ordinary file-backed `mmap` on a host advertising `HL_HOST_CAP_WATCH`, which is
  both Linux and macOS.

So "the dispatcher already guards other work with `g_threaded`" is not an argument
for guarding a handshake with it. Ask who the *reader* is: a signal handler is not
a thread and needs only a compiler barrier, a `fork`ed child shares no memory with
its parent, but a host service thread is a genuine second core and `g_threaded`
says nothing about it. The translator already learned the same lesson from the
other side -- `g_shared_obs` exists because `g_threaded`, being per-process, says
nothing about a peer *process* on a `MAP_SHARED` region.

## Application boundaries

`src/containers/hl-engine` is the engine composition root. It owns:

- public configuration and validation;
- CLI and environment capture;
- platform and execution-backend selection;
- concrete adapter construction;
- the supported Rust API;
- the opaque C ABI;
- packaging and target-specific linkage.

The engine wires capabilities and delegates behavior. It must not become the owner of
filesystem, descriptor, syscall, task, or execution algorithms.

`src/apps/husklet` is the product composition root. It owns product configuration,
GUI/CLI behavior, backend selection, and cross-domain orchestration. It must delegate
container, workspace, terminal, filesystem, and engine behavior to their owners rather
than becoming a service locator or god object.

## Tests

- Unit tests live beside the owning source.
- Crate `tests/` exercise only that crate's public contract.
- Repository `tests/` contains multi-package, process, hardware, application, and
  engine-in-engine tests.
- Tests are deterministic, isolated, bounded, and responsible for their resources.
- Fixes begin with a failing behavioral test when feasible.
- Differential tests run the same operation against C and Rust and compare results,
  errno, state, ownership, ordering, and serialized data.

A directory under `src/` must not exist only to aggregate detached test fragments.
When two or more Rust files in a source directory are all test-only, move each test
beside the production noun it exercises and prefer an inline `#[cfg(test)]` module.
Test code must not import behavior or fixtures from a sibling test module. Put
genuinely shared, behavior-free fixtures behind one explicitly declared
`test_support` module owned by the production boundary instead.

Required migration gates are:

1. formatting, design lint, Clippy with warnings denied, unit and documentation
   tests;
2. C/Rust ABI and differential tests;
3. both guest ISA compatibility and production tests;
4. checkpoint and cross-checkpoint;
5. native ARM64 macOS/Linux, AMD64 Linux, and AMD64 Windows target checks;
6. nested engine and performance tests;
7. ordinary container and interactive terminal workflows through Husklet.

### Reproducible Nix driver

`flake.lock` pins the development and verification toolchain. Use the flake as
the repository-level entry point:

```text
nix develop
nix build -L --option cores 0 --max-jobs auto
nix flake check -L --option cores 0 --max-jobs auto
```

Run Clippy and rustfmt inside `nix develop`, or let `nix flake check` run the
complete locked verification derivation. A bare `cargo clippy` fails with E0514;
the mechanism and the recovery are in "Clippy and rustfmt only work through the
pinned shell".

The default shell exposes both Linux guest compilers and the retained
`*_LINUX_CC`, `*_LINUX_STATIC_CC`, `*_DYNAMIC_LOADER`, and `*_DYNAMIC_LIBC`
contracts. Interactive verification must override conservative environment
defaults and size `CARGO_BUILD_JOBS` and `HL_COMPAT_JOBS` to the host's logical
CPU count unless measured RAM, disk, thermal, or lifecycle pressure requires a
lower bound. The named flake checks alias one comprehensive verification
derivation deliberately; use its internal parallelism rather than launching
duplicate full Cargo builds that contend for the same dependency graph.
The derivation must remain offline, locked, warning-strict, and responsible for
format, design lint, lint cases, workspace and documentation tests, and checked
compatibility metadata. Do not reintroduce retained-tree CMake, Ninja,
clang-tidy, or cppcheck command pipelines. Native C compilation is owned by
`hl-native/build.rs`; portable source analysis is embedded in `hl-design-lint`.

## A gate that cannot fail is worse than no gate

Five separate gates were found reporting success while proving nothing, in a
single day. Each had looked green for weeks or months.

- A `flake.nix` gate filtered for a test that had been **renamed**. `cargo test
  <filter> -- --exact` exits **0** when nothing matches: `running 0 tests /
  test result: ok / 182 filtered out / exit 0`. Both arms were vacuous.
- `src/apps/engine/tests/` holds exactly one file, gated
  `#![cfg(all(target_os = "linux", target_arch = "aarch64"))]`. On any other
  host the whole integration-test target compiles to zero tests and reports ok.
- `errno_namespace.rs::linux_namespace_values_are_not_a_valid_input_on_divergent_hosts`
  has a body of two `#[cfg]` arms, macOS and Windows. On Linux it is an empty
  function that passes.
- The `nix flake check` verification derivation had empty `buildInputs` while
  the workspace had grown crates needing gtk4 through pkg-config, so it failed
  before compiling anything -- for a week, on the step the macOS job is built
  around.
- Both GUI tests **skip when there is no display**, so a CI line reading
  "88 tests run" included two that assert nothing.

And the same shape in a diagnostic: `engine/target/x86_64.c` printed
`(IBTC %s)` with `"ON"` as a string literal, while the counter's only producer
sits behind `HL_HOST_CPU_AARCH64` -- a host with no JIT reported the feature as
on.

The rule: **a gate must be shown to fail.** Before trusting one, break the thing
it guards and watch it redden. When a suite reports a count, check the count is
what you expect -- a lane once ran twenty green rounds of a filter that matched
zero tests, and another swept 626 binaries that never started because the
harness ate its own first argument. Both caught it only because they had planted
a control that *had* to fail, and noticed it hadn't.

A skip is not a pass. If a test cannot run here, it must say so out loud: the
harness shows captured output only for failing tests, so an unrun arm otherwise
looks exactly like a passing one.

Counting a suite's result correctly -- the passed-versus-passed-plus-ignored
convention, which `test result:` line is the parent's, why `grep -c FAILED`
over-counts, how many runs a suite with a flaky member needs, and the stdin a
`-p hl-engine --tests` run inherits -- is its own subject. See **Counting and
measurement**.

### "Architecture-dependent" is a claim, and it needs the other architecture

Four `checkpoint_linux.rs` failures were carried for weeks as "aarch64-flavoured"
-- written on Apple silicon, failing on x86_64, therefore presumed to be about
the ISA. Nobody had ever run them on aarch64, because after the move to this
x86_64 box there was no aarch64 host to run them on.

Run on a real aarch64 guest with the JIT live -- 89 emitter bodies present in the
loaded `.so` against **0** on x86_64 -- **all four reproduce with byte-identical
panic text and identical store key sets.** They are not architecture-dependent;
they are broken everywhere. A second, cheaper control agrees: flipping the
fixture's ISA order to put x86_64 first reproduces all four on the *x86_64* guest
on this host, no VM required.

One of the four was then found to be unsatisfiable rather than merely failing --
it asserted that a refused capture leaves no socket queue behind, while handing
the engine a `Store` whose `abort_until` is `Ok(())`. **No engine behaviour could
have made it pass.** A test nobody can satisfy is indistinguishable, from the
outside, from a product defect nobody has fixed.

The rule: a failure is only "specific to X" once it has been run somewhere that
is not X. Until then "aarch64-flavoured" is a hypothesis wearing a label, and it
will keep four real bugs parked behind a plausible excuse. Look for the cheap
control first -- an ISA order flip found this in minutes; the VM only confirmed
it.

### The gate that reported nothing, and was read as if it had

The five above proved nothing while saying `ok`. This one said nothing at all,
which is harder to notice because there is no green line to disbelieve.

Measured 2026-08-20: the **fifteen most recent CI runs on `main` had all ended
`cancelled`** -- not one `success`, not one `failure`, going back through the
whole working day. `ci.yml` and `smoke.yml` carried `cancel-in-progress: true`,
nine lanes were merging into `main` roughly every ten minutes, and the workflow
needs about forty-five. Every run was killed by the next push. **No commit on
`main` had ever reached a verdict.**

Nobody had disabled anything, nothing was `#[ignore]`d, and every job was
correctly written. The branch simply had no test signal, and it looked from the
outside like a branch whose CI was running. When the cancellation was removed
and one run was allowed to finish, **five real reds surfaced at once** -- the
Windows MSVC surface, the macOS `nix flake check`, the macOS public C/C++ ABI
contracts, and all four legs of a `Real-image smoke` workflow nobody was even
tracking. Along with a genuine success on the new Linux job, which had also been
invisible.

The durable rules:

- **`cancel-in-progress: true` is wrong on an integration branch whenever the
  merge interval is shorter than the workflow duration.** It is right on pull
  requests, where a new head commit genuinely supersedes the one under test. On
  `main` it is a race the branch always loses. GitHub's queue is bounded at one
  without it -- a newly queued run cancels the previously *pending* run in the
  same group -- so declining to cancel does not pile up runners.
- **Read the distribution of conclusions, not the latest one.** A single
  `cancelled` is routine and means nothing. Fifteen in a row is a dead gate, and
  the two are indistinguishable if you only ever look at the most recent run.
  `conclusion` is the field to count; a run that is `completed` is not
  necessarily a run that decided anything.
- A gate can fail to produce evidence for reasons that have nothing to do with
  the code it tests. Ask what the last *verdict* was and when, not whether CI is
  configured.

## Design lint

`src/packages/hl-design-lint` is the repository architecture linter. Run it in
the pinned shell:

```text
cargo run --locked --offline -q -p hl-design-lint -- --policy lint.toml src tests
cargo run --locked --offline -q -p hl-design-lint -- --policy lint.toml --cases lint src tests
```

It enforces dependency direction and cycles, source ownership, ambient environment
access, platform-command boundaries, catch-all modules, oversized files, ceremonial
structure, and other reviewed design rules.

`lint/errors/` contains unclassified generated findings. `lint/check/` contains
temporarily classified findings. Both are review queues, not suppressions.

`lint/examples/positive.md` contains approved transformations.
`lint/examples/negative.md` contains rejected transformations and their failure
modes. The corpus began from Husklet's reviewed examples; engine-specific decisions
must be added as the rewrite exposes real cases.

### Lint-case protocol

Before resolving a generated lint case, read:

- this entire `AGENTS.md`;
- all of `lint/examples/positive.md`;
- all of `lint/examples/negative.md`;
- the current source, callers, sibling behavior, owning manifest, and nearby tests.

A generated case is evidence and may be stale. Refactor into the correct entity,
package, port, adapter, or inline behavior when ownership is clear. Do not add a
classification, allowance, dependency, wrapper, or empty abstraction merely to
make the queue pass.

Append a positive or negative example only after user approval. Preserve the
reasoning, not only the final code.

## Style

- Use precise nouns and domain vocabulary.
- Avoid `Manager`, `Helper`, `Util`, `Impl`, vague abbreviations, and repeated
  module prefixes.
- A trait or type is already a namespace; method names do not repeat it.
- Every word in a name must earn its place: drop any word the module, type, or
  trait already supplies. There is no word budget, because a name that spells an
  external mechanism — `oom_score_adj`, `write_life_hint`, `unix_socket_path` —
  spends its words on one concept, and that spelling is worth more than brevity.
  Never shorten or paraphrase a name that maps to a kernel, ABI, wire, or vendor
  spelling; the exact match is the documentation.
- Prefer standard conversion, parsing, formatting, and iterator traits when they
  express the complete contract.
- Keep the happy path shallow.
- Public APIs are minimal and document invariants, errors, safety, ownership, and
  non-obvious performance contracts.
- Comments explain contracts and reasons; names explain mechanics.
- Lint allowances are local and justified.
- Delete obsolete implementations after their migration and parity window passes.

## Delivery

Refactor incrementally. Every migration leaves an acyclic package graph and a
working production path. Temporary dependency cycles, permanent compatibility
shims, application-specific engine hacks, and parallel abandoned implementations
are not accepted migration strategies.
