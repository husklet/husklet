#!/usr/bin/env bash
# dd LTP compliance lane — RUN + SCORE stage (differential vs a native oracle).
#
# For every curated LTP binary built by build.sh, run it BOTH under dd's JIT and
# under the ground-truth oracle for that arch, classify each run into an LTP
# verdict {PASS,FAIL,BROK,CONF,CRASH,TIMEOUT} from the test's own Summary block +
# exit status, and diff them:
#   * oracle PASS & dd PASS               -> ok
#   * oracle PASS & dd !PASS              -> DD-GAP  (a real dd compliance gap)
#   * oracle !PASS (CONF/BROK/…)          -> skip    (no valid ground truth here;
#                                                      NOT counted against dd)
# The headline dd pass-rate is (ok / oracle-PASS tests), per arch.
#
# Oracle:  arm64 -> run the SAME static binary natively (this host is arm64);
#          x86_64 -> run it under qemu-x86_64 (user-mode).
# dd:      run the binary under $HL_JIT_DIR/hljit-linux_<arch> via the `mac`
#          bridge (the engine is a macOS arm64 binary; see hl-jit SpawnConfig).
#
# Requires: HL_JIT_DIR pinned to an engine out-dir (contains hljit-linux_{aarch64,
# x86_64}); qemu-x86_64 for the x86 oracle. Results: $OUT/results.tsv (+ stdout).
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${LTP_OUT:-$HERE/out}"
TIMEOUT="${LTP_TIMEOUT:-20}"
ARCHES="${LTP_ARCHES:-arm64 x86_64}"

if [ -z "${HL_JIT_DIR:-}" ]; then
  # Best-effort autodiscover: newest hljit-*/out under a sibling target*/ dir.
  HL_JIT_DIR="$(ls -dt "$HERE"/../../../target*/release/build/hljit-*/out 2>/dev/null | head -1)"
fi
[ -n "${HL_JIT_DIR:-}" ] && [ -d "$HL_JIT_DIR" ] || { echo "ERROR: pin HL_JIT_DIR to an engine out-dir"; exit 2; }
echo "[ltp] engine dir: $HL_JIT_DIR"

# On a Linux dev host the engine runs mac-side through the `mac` bridge; on a real
# Mac it runs directly. Mirror hl-jit::SpawnConfig::command.
run_dd() { # $1 engine  $2 guestbin
  # Blocking-syscall tests (select/pause/…) can hit the timeout. When they do, the
  # local `mac` client is killed but the mac-side engine LINGERS (orphan pileup —
  # see memory "reap mac-bridge orphans"), stalling every later test and forcing
  # transient failures. We can't broad-reap hljit engines by name: sibling agents
  # share this host and would be cross-killed. So reap SURGICALLY — the mac-side
  # bash `exec`s into the engine, inheriting bash's PID; we stash that one PID and,
  # only on timeout, kill exactly it (and its group) mac-side. Nothing else is touched.
  if [ "$(uname -s)" = "Darwin" ]; then
    timeout "$TIMEOUT" bash -lc "exec env '$1' '$2'"
  else
    local pidf="$OUT/.ddpid.$$"; : > "$pidf"
    timeout "$TIMEOUT" mac bash -lc "echo \$\$ > '$pidf'; exec env '$1' '$2'"
    local rc=$?
    if [ "$rc" = 124 ]; then
      local mpid; mpid="$(cat "$pidf" 2>/dev/null)"
      if [ -n "$mpid" ] 2>/dev/null && [ "$mpid" -gt 1 ] 2>/dev/null; then
        mac bash -lc "kill -TERM '$mpid' 2>/dev/null; kill -TERM -'$mpid' 2>/dev/null; sleep 0.2; kill -KILL '$mpid' 2>/dev/null; kill -KILL -'$mpid' 2>/dev/null" >/dev/null 2>&1 || true
      fi
    fi
    rm -f "$pidf"
    return $rc
  fi
}
run_oracle() { # $1 arch  $2 guestbin
  if [ "$1" = "x86_64" ]; then timeout "$TIMEOUT" qemu-x86_64 "$2"; else timeout "$TIMEOUT" "$2"; fi
}

# Count a run's per-assertion LTP result lines. Echoes "npass nfail nbrok nconf".
# We score on the test's OWN result lines (TPASS/TFAIL/TBROK/TCONF), not on the
# trailing "Summary:" block — because dd can run every assertion correctly yet
# crash in the harness's fork/exit TEARDOWN before the Summary prints (see the
# TEARDOWN handling in the loop). Counting result lines credits the syscall for
# what it actually did and isolates the teardown bug as its own signal.
counts() {
  local f="$1"
  printf '%s %s %s %s' \
    "$(grep -c 'TPASS' "$f")" "$(grep -c 'TFAIL' "$f")" \
    "$(grep -c 'TBROK' "$f")" "$(grep -c 'TCONF' "$f")"
}
# Reduce (np nf nb nc rc) to a verdict word.
verdict() {
  local np="$1" nf="$2" nb="$3" nc="$4" rc="$5"
  if   [ "$nf" -gt 0 ]; then echo FAIL
  elif [ "$nb" -gt 0 ]; then echo BROK
  elif [ "$np" -gt 0 ]; then echo PASS
  elif [ "$nc" -gt 0 ]; then echo CONF
  elif [ "$rc" = 124 ]; then echo TIMEOUT
  elif [ "$rc" -ge 128 ] 2>/dev/null || [ "$rc" = 255 ]; then echo CRASH
  else echo BROK; fi
}

RES="$OUT/results.tsv"
printf 'arch\tcategory\ttest\toracle\tdd\tstatus\tdd_exit\n' > "$RES"
tmp_o="$OUT/.o.log"; tmp_d="$OUT/.d.log"

declare -A CAT_OF SYS_OF
while read -r cat sys rel; do
  case "$cat" in ''|\#*) continue;; esac
  n="$(basename "$rel" .c)"; CAT_OF["$n"]="$cat"; SYS_OF["$n"]="$sys"
done < "$HERE/tests.list"

for arch in $ARCHES; do
  eng="$HL_JIT_DIR/hljit-linux_$([ "$arch" = x86_64 ] && echo x86_64 || echo aarch64)"
  [ -x "$eng" ] || { echo "[ltp] no engine for $arch ($eng) — skipping arch"; continue; }
  bd="$OUT/bin/$arch"
  [ -d "$bd" ] || { echo "[ltp] no built tests for $arch — run build.sh"; continue; }
  echo "=== arch $arch (engine $(basename "$eng")) ==="
  for bin in "$bd"/*; do
    [ -f "$bin" ] || continue
    case "$bin" in *.err) continue;; esac
    name="$(basename "$bin")"
    # Optional space-separated allowlist of test names for a targeted re-run.
    if [ -n "${LTP_ONLY:-}" ]; then case " $LTP_ONLY " in *" $name "*) ;; *) continue;; esac; fi
    cat="${CAT_OF[$name]:-?}"
    run_oracle "$arch" "$bin" >"$tmp_o" 2>&1; orc=$?
    read -r op of ob oc <<<"$(counts "$tmp_o")"
    ov=$(verdict "$op" "$of" "$ob" "$oc" "$orc")
    # dd run — with transient-failure retry. The macOS engine is spawned per test
    # through the `mac` bridge; a fork/clone-heavy neighbour can transiently
    # saturate the host so an *unrelated* launch dies with no result lines (a
    # known orphan-engine pileup, see memory "reap mac-bridge orphans"). A REAL
    # dd bug reproduces deterministically; a host-overload artifact clears. So
    # when dd produced NO result lines (CRASH/TIMEOUT/BROK), re-run up to 3x and
    # keep the best; a run that emitted TPASS/TFAIL lines is taken as final.
    dv=""; drc=0; dp=0; df=0; db=0; dc=0
    for attempt in 1 2 3; do
      run_dd "$eng" "$bin" >"$tmp_d" 2>&1; drc=$?
      read -r dp df db dc <<<"$(counts "$tmp_d")"
      dv=$(verdict "$dp" "$df" "$db" "$dc" "$drc")
      case "$dv" in PASS|FAIL|CONF) break;; esac
      sleep 0.5
    done
    # Diff. Only oracle-PASS tests score dd. Among those:
    #   dd FAIL/BROK/CRASH/TIMEOUT            -> DD-GAP   (a real syscall gap)
    #   dd PASS but ran FEWER assertions      -> DD-GAP   (truncated: died mid-test)
    #   dd PASS, all assertions, but exit!=0  -> TEARDOWN (assertions correct; a
    #                                            systemic harness fork/exit-path
    #                                            crash — one root cause, reported
    #                                            apart from per-syscall gaps)
    #   dd PASS, all assertions, exit==oracle -> ok
    if [ "$ov" != PASS ]; then status=skip
    elif [ "$dv" = PASS ]; then
      if [ "$dp" -lt "$op" ]; then status=DD-GAP
      elif [ "$drc" = "$orc" ]; then status=ok
      else status=TEARDOWN; fi
    else status=DD-GAP; fi
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$arch" "$cat" "$name" "$ov" "$dv" "$status" "$drc" >> "$RES"
    printf '  %-8s %-26s oracle=%-7s dd=%-7s %s\n' "$cat" "$name" "$ov" "$dv" "$status"
  done
done
rm -f "$tmp_o" "$tmp_d"

# ---- scorecard ---------------------------------------------------------------
echo
echo "================ dd LTP compliance scorecard ================"
for arch in $ARCHES; do
  tot=$(awk  -F'\t' -v a="$arch" '$1==a && $6!="skip"{n++} END{print n+0}' "$RES")
  ok=$(awk   -F'\t' -v a="$arch" '$1==a && $6=="ok"{n++} END{print n+0}' "$RES")
  td=$(awk   -F'\t' -v a="$arch" '$1==a && $6=="TEARDOWN"{n++} END{print n+0}' "$RES")
  gap=$(awk  -F'\t' -v a="$arch" '$1==a && $6=="DD-GAP"{n++} END{print n+0}' "$RES")
  skp=$(awk  -F'\t' -v a="$arch" '$1==a && $6=="skip"{n++} END{print n+0}' "$RES")
  syspass=$((ok+td))
  spct=$(awk -v o="$syspass" -v t="$tot" 'BEGIN{ if(t>0) printf "%.1f", 100*o/t; else print "n/a"}')
  cpct=$(awk -v o="$ok"      -v t="$tot" 'BEGIN{ if(t>0) printf "%.1f", 100*o/t; else print "n/a"}')
  printf 'hl-%-7s  syscall-assertion PASS %s/%s (%s%%)   clean-run %s/%s (%s%%)   [%s teardown-only, %s real gaps, %s oracle-nonpass excluded]\n' \
    "$arch" "$syspass" "$tot" "$spct" "$ok" "$tot" "$cpct" "$td" "$gap" "$skp"
done
echo "  syscall-assertion PASS = every TPASS/TFAIL assertion matches the oracle (credits teardown-only)."
echo "  clean-run              = also exits byte-identically to the oracle (excludes the teardown crash)."
echo
echo "Per-category (arch: syscall-PASS/oracle-PASS):"
awk -F'\t' '$6!="skip"{tot[$1"|"$2]++; if($6=="ok"||$6=="TEARDOWN")ok[$1"|"$2]++}
  END{for(k in tot){split(k,a,"|"); printf "  %-8s %-10s %d/%d\n", a[1], a[2], ok[k]+0, tot[k]}}' "$RES" | sort
echo
echo "TEARDOWN (assertions PASS but dd exits nonzero — systemic harness fork/exit gap):"
awk -F'\t' '$6=="TEARDOWN"{printf "  %-8s %-12s %-24s hl_exit=%s\n",$1,$2,$3,$7}' "$RES" | sort || true
echo
echo "DD-GAP (dd assertions != oracle — real per-syscall compliance gaps):"
awk -F'\t' '$6=="DD-GAP"{printf "  %-8s %-12s %-24s oracle=%s dd=%s exit=%s\n",$1,$2,$3,$4,$5,$7}' "$RES" | sort || true
echo "============================================================="
echo "[ltp] full table: $RES"
