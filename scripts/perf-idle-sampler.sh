#!/bin/bash
APP="/Users/chmod777/myProject/doing-tauri/target/release/bundle/macos/Doing.app"
pkill -f "bundle/macos/Doing.app/Contents/MacOS/doing-desktop" 2>/dev/null; sleep 1
open "$APP"; sleep 8
echo "phase,time,main_rss_kb,main_cpu,webkit_rss_kb,webkit_cpu" > /tmp/perf-idle.log
START=$(date +%s)
for i in $(seq 1 20); do
  PID=$(pgrep -f "bundle/macos/Doing.app/Contents/MacOS/doing-desktop" | head -1)
  M=$(ps -o rss=,%cpu= -p "$PID" 2>/dev/null | awk '{print $1","$2}')
  W=$(pgrep -f "WebKit.WebContent" | head -4 | tr '\n' ',' | sed 's/,$//')
  WS=$(ps -o rss=,%cpu= -p "$W" 2>/dev/null | awk '{s+=$1;c+=$2} END{print s","c}')
  T=$(( $(date +%s) - START ))
  echo "idle,$T,$M,$WS" >> /tmp/perf-idle.log
  sleep 15
done
pkill -f "bundle/macos/Doing.app/Contents/MacOS/doing-desktop" 2>/dev/null
echo "done" >> /tmp/perf-idle.log
