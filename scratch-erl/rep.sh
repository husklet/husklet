#!/bin/bash
BIN=/Users/x/dd/dd/scratch-erl/ddjit-fix
ROOTFS=/Users/x/dd/dd/scratch-erl/rootfs
cd /Users/x/dd/dd/scratch-erl
N=${1:-20}
: > summary.log
pass=0; fail=0
for i in $(seq 1 $N); do
  rm -f erl_crash.dump
  timeout 90 env DDDBG_ENGFAULT=1 DD_HOSTNAME=erlbox "$BIN" --rootfs "$ROOTFS" \
    /bin/sh -c 'export PATH=/usr/local/bin:/usr/bin:/bin; erl -noshell -eval "io:format(\"BOOTOK~n\"), halt()."' \
    > run_$i.log 2>&1
  ec=$?
  if grep -q BOOTOK run_$i.log && [ $ec -eq 0 ]; then
    echo "run $i PASS ec=$ec" >> summary.log; pass=$((pass+1))
  else
    echo "run $i FAIL ec=$ec :: $(grep -iE 'ENGFAULT|CRASH|Failed to write|SIG|Aborted' run_$i.log | head -3 | tr '\n' '~')" >> summary.log; fail=$((fail+1))
  fi
done
echo "TOTAL pass=$pass fail=$fail" >> summary.log
