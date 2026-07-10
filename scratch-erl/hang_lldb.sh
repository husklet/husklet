#!/bin/bash
BIN=/Users/x/dd/dd/scratch-erl/ddjit-clean
ROOTFS=/Users/x/dd/dd/scratch-erl/rootfs
cd /Users/x/dd/dd/scratch-erl
: > lldb.log
for i in $(seq 1 12); do
  rm -f erl_crash.dump
  env DD_HOSTNAME=erlbox "$BIN" --rootfs "$ROOTFS" \
    /bin/sh -c 'export PATH=/usr/local/bin:/usr/bin:/bin; erl -noshell -eval "os:cmd(\"true\"), os:cmd(\"echo hi\"), io:format(\"FORKOK~n\"), halt()."' \
    > tl_$i.log 2>&1 &
  gpid=$!
  ok=0
  for t in $(seq 1 20); do
    grep -q FORKOK tl_$i.log 2>/dev/null && { ok=1; break; }
    kill -0 $gpid 2>/dev/null || break
    sleep 1
  done
  if [ $ok -eq 1 ]; then echo "try $i PASS" >> lldb.log; wait $gpid 2>/dev/null; continue; fi
  echo "try $i HANG" >> lldb.log
  # find the spinning (R state) ddjit-clean child
  for p in $(pgrep -f ddjit-clean); do
    st=$(ps -o state= -p $p 2>/dev/null)
    echo "==== pid $p state=$st ====" >> lldb.log
    if [[ "$st" == R* ]]; then
      lldb -p $p -o "thread list" -o "bt" -o "register read sp pc" \
        -o "memory region $sp" -o "image lookup -a $pc" -o "register read" -o "thread info" -o "detach" -o "quit" \
        --batch >> lldb.log 2>&1
    fi
  done
  kill -9 $gpid 2>/dev/null; pkill -9 -f ddjit-clean 2>/dev/null
  echo "CAPTURED try $i" >> lldb.log
  break
done
echo DONE >> lldb.log
