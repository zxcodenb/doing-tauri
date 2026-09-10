#!/bin/bash
# Doing 冷启动/唤起初测（macOS，release 包）：
# 1) cold_to_tray_ready_ms：进程从启动到菜单栏托盘项可访问（菜单栏应用的“就绪”口径）
# 2) second_instance_to_window_ms：已运行时再次启动（单实例转发→present_main）到主窗口出现
# 说明：非 P0 正式口径；正式测量需 ≥30 次且区分首次 WebView 初始化。
APP="/Users/chmod777/myProject/doing-tauri/target/release/bundle/macos/Doing.app"
LOG=/tmp/perf-cold.log
RUNS=${1:-5}
echo "phase,run,ms" > "$LOG"

tray_ready() {
  osascript -e 'tell application "System Events" to tell process "doing-desktop" to get position of menu bar item 1 of menu bar 2' 2>/dev/null | grep -qE '^[0-9-]+, *[0-9-]+$'
}
window_ready() {
  N=$(osascript -e 'tell application "System Events" to tell process "doing-desktop" to get count of windows' 2>/dev/null)
  case "$N" in ''|*[!0-9]*) return 1 ;; *) [ "$N" -ge 1 ] ;; esac
}
wait_for() { # $1=func $2=deadline_secs
  local end=$(( $(date +%s) + $2 ))
  while [ "$(date +%s)" -lt "$end" ]; do
    if "$1"; then return 0; fi
    sleep 0.1
  done
  return 1
}
now_ms() { python3 -c 'import time;print(int(time.time()*1000))'; }

for i in $(seq 1 "$RUNS"); do
  pkill -f "bundle/macos/Doing.app/Contents/MacOS/doing-desktop" 2>/dev/null
  sleep 2
  T0=$(now_ms); open "$APP"
  wait_for tray_ready 30; T1=$(now_ms)
  echo "cold_to_tray_ready,$i,$((T1-T0))" >> "$LOG"

  T0=$(now_ms); open -n "$APP"
  wait_for window_ready 10; T1=$(now_ms)
  echo "second_instance_to_window,$i,$((T1-T0))" >> "$LOG"
done
pkill -f "bundle/macos/Doing.app/Contents/MacOS/doing-desktop" 2>/dev/null
echo "phase,done," >> "$LOG"
