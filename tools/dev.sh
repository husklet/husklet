#!/usr/bin/env bash
# dev.sh -- build dd and set it up so you can open a shell and just use `hl`.
#
#   bash tools/dev.sh
#
# Builds hl-daemon + hl, puts `hl` on your PATH (~/.local/bin, added to your shell rc), and runs
# the daemon in the background. Then open a NEW terminal window and run `hl ubuntu`.
#
# Env: HL_IMAGES (image dir, default ~/.hl/images -- pulls land here on demand).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SOCK="$HOME/.hl/run/docker.sock"
IMAGES="${HL_IMAGES:-$HOME/.hl/images}"
LOG="$HOME/.hl/daemon.log"
BIN="$HOME/.local/bin"
mkdir -p "$HOME/.hl/run" "$IMAGES" "$BIN"

echo "==> building hl-daemon + hl (release) ..."
cargo build --release -p hl-daemon -p husklet

echo "==> putting hl on your PATH ($BIN/hl)"
ln -sf "$ROOT/target/release/hl" "$BIN/hl"
# Ensure ~/.local/bin is on PATH for future shells. zsh is the macOS default; cover bash too.
PATHLINE='export PATH="$HOME/.local/bin:$PATH"'
added=""
for rc in "$HOME/.zshrc" "$HOME/.bashrc"; do
  [ -e "$rc" ] || continue
  if ! grep -qF "$PATHLINE" "$rc"; then
    printf '\n# dd: hl lives here\n%s\n' "$PATHLINE" >> "$rc"
    added="$added $rc"
  fi
done
# Fresh macOS with no rc yet: create ~/.zshrc.
if [ ! -e "$HOME/.zshrc" ] && [ ! -e "$HOME/.bashrc" ]; then
  printf '# dd: hl lives here\n%s\n' "$PATHLINE" > "$HOME/.zshrc"
  added=" $HOME/.zshrc"
fi
[ -n "$added" ] && echo "    added ~/.local/bin to PATH in:$added"

echo "==> (re)starting the daemon in the background (log: $LOG)"
pkill -x hl-daemon 2>/dev/null || true
rm -f "$SOCK"
HL_DOCKER_SOCK="$SOCK" HL_IMAGES="$IMAGES" nohup "$ROOT/target/release/hl-daemon" >"$LOG" 2>&1 &
disown 2>/dev/null || true
for _ in $(seq 1 40); do [ -S "$SOCK" ] && break; sleep 0.25; done
[ -S "$SOCK" ] || { echo "daemon failed to start; see $LOG"; tail -20 "$LOG"; exit 1; }

cat <<EOF

────────────────────────────────────────────────────────────────────
  ✓ hl is on your PATH and the daemon is running.

  Open a NEW terminal window and just use it:

      hl ubuntu             # a shell in ubuntu, here in your current dir
      hl run alpine echo hi
      hl run alpine uname -m

  (Already-open shells won't see the PATH change — open a fresh window,
   or run:  export PATH="\$HOME/.local/bin:\$PATH")

  Daemon log:  tail -f $LOG       Stop it:  pkill -x hl-daemon
────────────────────────────────────────────────────────────────────
EOF
