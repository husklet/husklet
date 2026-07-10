#!/bin/bash
BIN=/Users/x/dd/dd/scratch-erl/ddjit-diag2
ROOTFS=/Users/x/dd/dd/scratch-erl/rootfs
cd /Users/x/dd/dd/scratch-erl
: > diag2.log
for i in $(seq 1 40); do
  rm -f erl_crash.dump
  out=$(env DDDBG_ENGFAULT=1 DD_HOSTNAME=erlbox "$BIN" --rootfs "$ROOTFS" \
    /bin/sh -c 'export PATH=/usr/local/bin:/usr/bin:/bin; erl -noshell -eval "os:cmd(\"true\"), os:cmd(\"echo hi\"), io:format(\"FORKOK~n\"), halt()."' 2>&1)
  ef=$(echo "$out" | grep -E "ENGFAULT")
  if echo "$out" | grep -q FORKOK && [ -z "$ef" ]; then echo "try $i PASS" >> diag2.log
  else echo "try $i FAIL :: $ef :: $(echo "$out" | grep -iE 'Failed to write' | head -1)" >> diag2.log; fi
done
echo DONE >> diag2.log
