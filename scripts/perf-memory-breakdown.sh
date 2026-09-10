#!/usr/bin/env bash
# 内存拆分采样（计划 D08）：主进程 + 可归属 WebView 进程的物理内存拆分
# （phys_footprint / 峰值），补充单纯 RSS 数字。
#
# 归属方法：记录启动前的 WebKit XPC 进程集合，启动应用后取差集，
# 属于本次启动的 WebContent / Networking / GPU 进程即本应用所有。
#
# 用法：scripts/perf-memory-breakdown.sh [app 路径，默认 target/release/bundle/macos/Doing.app]
set -euo pipefail
cd "$(dirname "$0")/.."

APP="${1:-target/release/bundle/macos/Doing.app}"
SETTLE="${SETTLE:-30}"

webkit_pids() {
  ps -axo pid,comm | grep -E "WebKit\.(WebContent|Networking|GPU)\.xpc" | awk '{print $1}' | sort
}

describe() {
  local pid="$1" label="$2"
  [ -d "/proc/$pid" ] || true
  if ! ps -p "$pid" >/dev/null 2>&1; then return; fi
  echo "── $label (pid=$pid)"
  ps -o comm= -p "$pid" | sed 's/^/   /'
  # 精简采样：物理占用与峰值（共享/私有明细见 vmmap -summary 或 Instruments）。
  if command -v footprint >/dev/null 2>&1; then
    footprint -p "$pid" 2>/dev/null | grep -Ei "phys_footprint" | head -3 | sed 's/^/   /' || true
  fi
  ps -o rss= -p "$pid" | awk '{printf "   rss: %.1f MB\n", $1/1024}'
  echo
}

before="$(webkit_pids)"
echo "启动前 WebKit 进程：$(echo "$before" | wc -l | tr -d ' ') 个"
open "$APP"
sleep "$SETTLE"

main_pids=$(pgrep -f "$APP" || true)
if [ -z "$main_pids" ]; then
  echo "启动失败：未找到 $APP 进程" >&2
  exit 1
fi

after="$(webkit_pids)"
new_webkit="$(comm -13 <(echo "$before") <(echo "$after"))"

for pid in $main_pids; do
  describe "$pid" "主进程（Rust 壳）"
done
for pid in $new_webkit; do
  comm=$(ps -o comm= -p "$pid" || true)
  case "$comm" in
    *WebContent*) describe "$pid" "WebView 渲染进程（本次启动新增）" ;;
    *Networking*) describe "$pid" "WebView 网络进程（本次启动新增）" ;;
    *GPU*) describe "$pid" "WebView GPU 进程（本次启动新增）" ;;
  esac
done

echo "（差集归属口径：仅统计本次启动新增的 WebKit XPC 进程；settle=${SETTLE}s）"
