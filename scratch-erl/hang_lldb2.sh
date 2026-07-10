#!/bin/bash
BIN=/Users/x/dd/dd/scratch-erl/ddjit-clean
ROOTFS=/Users/x/dd/dd/scratch-erl/rootfs
cd /Users/x/dd/dd/scratch-erl
: > lldb2.log
for i in $(seq 1 15); do
  rm -f erl_crash.dump
  env DD_HOSTNAME=erlbox "$BIN" --rootfs "$ROOTFS" \
    /bin/sh -c 'export PATH=/usr/local/bin:/usr/bin:/bin; erl -noshell -eval "os:cmd(\"true\"), os:cmd(\"echo hi\"), io:format(\"FORKOK~n\"), halt()."' \
    > t2_$i.log 2>&1 &
  gpid=$!
  ok=0
  for t in $(seq 1 18); do
    grep -q FORKOK t2_$i.log 2>/dev/null && { ok=1; break; }
    kill -0 $gpid 2>/dev/null || break
    sleep 1
  done
  if [ $ok -eq 1 ]; then echo "try $i PASS" >> lldb2.log; wait $gpid 2>/dev/null; continue; fi
  echo "try $i HANG" >> lldb2.log
  ps -A -o pid,ppid,%cpu,state,command | grep -E 'ddjit-clean' | grep -v grep >> lldb2.log
  # attach lldb to EVERY ddjit-clean process
  for p in $(pgrep -f ddjit-clean); do
    echo "########## lldb pid $p ##########" >> lldb2.log
    lldb -p $p --batch \
      -o "bt" \
      -o "register read sp pc x28" \
      -o "memory region \$sp" \
      -o "image lookup -a \$pc" \
      -o "detach" -o "quit" >> lldb2.log 2>&1
  done
  kill -9 $gpid 2>/dev/null; pkill -9 -f ddjit-clean 2>/dev/null
  echo "CAPTURED try $i" >> lldb2.log
  break
done
echo DONE >> lldb2.log
