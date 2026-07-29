#!/bin/bash
# measure_single.sh - run pattern=single while sampling RSS of agentd owning :8090
cd /root/workspace/agentOS || exit 1

PID=$(ss -ltnp 2>/dev/null | grep ':8090' | grep -oE 'pid=[0-9]+' | head -1 | cut -d= -f2)
if [ -z "$PID" ]; then echo "NO_AGENTD_ON_8090"; exit 2; fi
echo "SERVING_PID=$PID"
echo "RSS_BEFORE_KB=$(ps -o rss= -p $PID | tr -d ' ')"

SID=$(curl -s -X POST http://127.0.0.1:8090/api/sessions | grep -oE '"session_id":"[^"]+"' | cut -d'"' -f4)
echo "SESSION=$SID"

( curl -s -N -X POST "http://127.0.0.1:8090/api/sessions/$SID/run" \
    -H 'Content-Type: application/json' \
    -d '{"pattern":"single","input":"用一句话介绍太阳系"}' \
    > /tmp/single_run.log 2>&1; echo "CURL_RC=$?" >> /tmp/single_run.log ) &
TPID=$!

PEAK=0
for i in $(seq 1 60); do
  RSS=$(ps -o rss= -p $PID 2>/dev/null | tr -d ' ')
  if [ -z "$RSS" ]; then echo "t=$((i*2))s AGENTD_DIED"; break; fi
  [ "$RSS" -gt "$PEAK" ] && PEAK=$RSS
  echo "t=$((i*2))s rss_kb=$RSS"
  if ! kill -0 $TPID 2>/dev/null; then echo "TEST_FINISHED at t=$((i*2))s"; break; fi
  sleep 2
done
echo "PEAK_RSS_KB=$PEAK"
echo "=== single_run.log (first 15 lines) ==="
head -15 /tmp/single_run.log
echo "=== single_run.log (last 5 lines) ==="
tail -5 /tmp/single_run.log
