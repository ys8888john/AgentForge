#!/bin/bash
# measure_diag.sh - rerun tool_use test; when RSS starts exploding, capture
# what agentd is actually doing (CPU time, threads, sockets, daemon log).
cd /root/workspace/agentOS || exit 1

PID=$(ss -ltnp 2>/dev/null | grep ':8090' | grep -oE 'pid=[0-9]+' | head -1 | cut -d= -f2)
if [ -z "$PID" ]; then echo "NO_AGENTD_ON_8090"; exit 2; fi
echo "SERVING_PID=$PID"

( bash tooluse_test2.sh > /tmp/tooluse_run3.log 2>&1 ) &
TPID=$!

PREV_UT=0
CAPTURED=0
for i in $(seq 1 45); do
  RSS=$(ps -o rss= -p $PID 2>/dev/null | tr -d ' ')
  [ -z "$RSS" ] && { echo "t=$((i*2))s AGENTD_DIED"; break; }
  UT=$(awk '{print $14+$15}' /proc/$PID/stat 2>/dev/null)
  TH=$(grep Threads /proc/$PID/status 2>/dev/null | tr -d ' \t')
  echo "t=$((i*2))s rss_kb=$RSS cpu_ticks=$UT $TH"
  # when RSS exceeds 1GB and not yet captured, take a deep snapshot
  if [ "$RSS" -gt 1000000 ] && [ "$CAPTURED" = "0" ]; then
    CAPTURED=1
    echo "---- SNAPSHOT at t=$((i*2))s ----"
    echo "-- sockets of agentd (to ollama:11434 and from clients) --"
    ss -tnp 2>/dev/null | grep "pid=$PID" | head -10
    echo "-- smaps_rollup --"
    grep -E 'Rss|AnonHugePages|Private' /proc/$PID/smaps_rollup 2>/dev/null | head -6
    echo "-- per-thread cpu (top 5) --"
    ps -L -o tid,pcpu,comm -p $PID --sort=-pcpu 2>/dev/null | head -6
    echo "-- daemon.log tail --"
    tail -5 /tmp/daemon.log 2>/dev/null
    echo "---- END SNAPSHOT ----"
  fi
  # second snapshot at >8GB to compare cpu ticks
  if [ "$RSS" -gt 8000000 ] && [ "$CAPTURED" = "1" ]; then
    CAPTURED=2
    echo "---- SNAPSHOT2 at t=$((i*2))s ----"
    ss -tnp 2>/dev/null | grep "pid=$PID" | head -10
    ps -L -o tid,pcpu,comm -p $PID --sort=-pcpu 2>/dev/null | head -6
    echo "---- END SNAPSHOT2 ----"
    # kill the test & agentd BEFORE OOM wrecks WSL
    echo "PREEMPTIVE_KILL to protect WSL"
    kill $TPID 2>/dev/null
    kill -9 $PID 2>/dev/null
    break
  fi
  if ! kill -0 $TPID 2>/dev/null; then echo "TEST_FINISHED at t=$((i*2))s"; break; fi
  sleep 2
done

echo "=== tooluse_run3.log ==="
wc -l /tmp/tooluse_run3.log; tail -25 /tmp/tooluse_run3.log
echo "=== daemon.log tail ==="
tail -10 /tmp/daemon.log 2>/dev/null
