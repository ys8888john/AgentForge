#!/bin/bash
# measure_run.sh - run tool_use test while sampling RSS of the agentd owning :8090
cd /root/workspace/agentOS || exit 1
cp /mnt/c/Users/Administrator/WorkBuddy/2026-07-29-00-21-29/tooluse_test2.sh . 2>/dev/null

PID=$(ss -ltnp 2>/dev/null | grep ':8090' | grep -oE 'pid=[0-9]+' | head -1 | cut -d= -f2)
if [ -z "$PID" ]; then echo "NO_AGENTD_ON_8090"; exit 2; fi
echo "SERVING_PID=$PID"
echo "RSS_BEFORE_KB=$(ps -o rss= -p $PID | tr -d ' ')"

( bash tooluse_test2.sh > /tmp/tooluse_run2.log 2>&1; echo "TEST_RC=$?" >> /tmp/tooluse_run2.log ) &
TPID=$!

PEAK=0
for i in $(seq 1 100); do
  RSS=$(ps -o rss= -p $PID 2>/dev/null | tr -d ' ')
  if [ -z "$RSS" ]; then echo "t=$((i*2))s AGENTD_DIED"; break; fi
  [ "$RSS" -gt "$PEAK" ] && PEAK=$RSS
  echo "t=$((i*2))s rss_kb=$RSS"
  if ! kill -0 $TPID 2>/dev/null; then echo "TEST_FINISHED at t=$((i*2))s"; break; fi
  sleep 2
done
echo "PEAK_RSS_KB=$PEAK"
RSS_AFTER=$(ps -o rss= -p $PID 2>/dev/null | tr -d ' ')
echo "RSS_AFTER_KB=${RSS_AFTER:-DEAD}"
echo "=== tooluse_run2.log (tail 40) ==="
tail -40 /tmp/tooluse_run2.log
echo "=== daemon.log (tail 15) ==="
tail -15 /tmp/daemon.log 2>/dev/null
