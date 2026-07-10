#!/bin/bash
BIN=/Users/x/dd/dd/scratch-erl/ddjit-clean
ROOTFS=/Users/x/dd/dd/scratch-erl/rootfs
cd /Users/x/dd/dd/scratch-erl
: > crashrep.log
DR=~/Library/Logs/DiagnosticReports
before=$(ls -1 $DR/ddjit-clean* 2>/dev/null | wc -l)
for i in $(seq 1 40); do
  rm -f erl_crash.dump
  out=$(env DD_HOSTNAME=erlbox "$BIN" --rootfs "$ROOTFS" \
    /bin/sh -c 'export PATH=/usr/local/bin:/usr/bin:/bin; erl -noshell -eval "os:cmd(\"true\"), os:cmd(\"echo hi\"), io:format(\"FORKOK~n\"), halt()."' 2>&1)
  if echo "$out" | grep -q FORKOK; then echo "try $i PASS" >> crashrep.log
  else echo "try $i FAIL :: $(echo "$out" | grep -iE 'Failed|Crash|error' | head -1)" >> crashrep.log; fi
  after=$(ls -1 $DR/ddjit-clean* 2>/dev/null | wc -l)
  if [ "$after" -gt "$before" ]; then
    newrep=$(ls -t $DR/ddjit-clean* 2>/dev/null | head -1)
    echo "==== NEW CRASH REPORT: $newrep ====" >> crashrep.log
    break
  fi
done
echo "DONE (reports: $(ls -1 $DR/ddjit-clean* 2>/dev/null | wc -l))" >> crashrep.log
