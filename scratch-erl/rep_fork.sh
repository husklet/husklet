#!/bin/bash
BIN=/Users/x/dd/dd/scratch-erl/ddjit-fix
ROOTFS=/Users/x/dd/dd/scratch-erl/rootfs
cd /Users/x/dd/dd/scratch-erl
N=${1:-40}
: > summary_fork.log
pass=0; fail=0
for i in $(seq 1 $N); do
  rm -f erl_crash.dump
  timeout 90 env DDDBG_ENGFAULT=1 DD_HOSTNAME=erlbox "$BIN" --rootfs "$ROOTFS" \
    /bin/sh -c 'export PATH=/usr/local/bin:/usr/bin:/bin; erl -noshell -eval "P=spawn(fun()->ok end), os:cmd(\"true\"), os:cmd(\"echo hi\"), io:format(\"FORKOK ~p~n\",[P]), halt()."' \
    > runf_$i.log 2>&1
  ec=$?
  if grep -q FORKOK runf_$i.log && [ $ec -eq 0 ]; then
    echo "run $i PASS ec=$ec" >> summary_fork.log; pass=$((pass+1))
  else
    echo "run $i FAIL ec=$ec :: $(grep -iE 'ENGFAULT|CRASH|Failed to write|SIG|Aborted|error' runf_$i.log | head -3 | tr '\n' '~')" >> summary_fork.log; fail=$((fail+1))
  fi
done
echo "TOTAL pass=$pass fail=$fail" >> summary_fork.log
