#!/bin/bash
BIN=/Users/x/dd/dd/scratch-erl/ddjit-fix
ROOTFS=/Users/x/dd/dd/scratch-erl/rootfs
cd /Users/x/dd/dd/scratch-erl
: > hang.log
for i in $(seq 1 12); do
  rm -f erl_crash.dump
  env DDDBG_ENGFAULT=1 DD_HOSTNAME=erlbox "$BIN" --rootfs "$ROOTFS" \
    /bin/sh -c 'export PATH=/usr/local/bin:/usr/bin:/bin; erl -noshell -eval "os:cmd(\"true\"), os:cmd(\"echo hi\"), io:format(\"FORKOK~n\"), halt()."' \
    > try_$i.log 2>&1 &
  gpid=$!
  # wait up to 25s for FORKOK
  ok=0
  for t in $(seq 1 25); do
    if grep -q FORKOK try_$i.log 2>/dev/null; then ok=1; break; fi
    if ! kill -0 $gpid 2>/dev/null; then break; fi
    sleep 1
  done
  if [ $ok -eq 1 ]; then
    echo "try $i PASS" >> hang.log
    wait $gpid 2>/dev/null
  else
    echo "try $i HANG — sampling" >> hang.log
    # sample every ddjit-fix process
    for p in $(pgrep -f ddjit-fix); do
      echo "==== SAMPLE pid $p ====" >> hang.log
      sample $p 2 -mayDie >> hang.log 2>&1
    done
    echo "==== process tree ====" >> hang.log
    ps -A -o pid,ppid,stat,command | grep -E 'ddjit-fix|beam|erl_child|[e]rl ' >> hang.log 2>&1
    kill -9 $gpid 2>/dev/null
    pkill -9 -f ddjit-fix 2>/dev/null
    echo "CAPTURED try $i" >> hang.log
    break
  fi
done
echo DONE >> hang.log
