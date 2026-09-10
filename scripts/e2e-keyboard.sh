#!/usr/bin/env bash
# 真实 WebView 键盘交互记录（计划 P2 交付物）——**当前状态：实验性，未通过**。
#
# 实测结论（2026-09-10，macOS + WKWebView）：
# - 合成按键（AppleScript keystroke / key code）对应用窗口部分有效：
#   ⌘N 聚焦录入框 ✓、字母数字输入 ✓、⌘A/⌫ 清空 ✓、Esc ✓、⌘, 打开设置 ✓；
# - **Return（key code 36 / ASCII 13）不触发录入框提交**（事件未送达 WebView 输入框）；
# - **CJK 字符无法注入**（System Events 对不可映射字符逐字回退为 'a'）。
# 因此本脚本当前只能验证“输入落框/Escape/导航类按键”，录入提交需真实键盘人工补验。
# 保留脚本作为后续排查基础（例如改用 CGEvent 直接投递 Return，或排查 WKWebView 键映射）。
#
# 对运行中的 dev 构建逐项尝试：录入 → Escape 清草稿 → ↑↓ 选择 → 空格完成 → ⌘Z 撤销 → ⌫ 删除 → ⌘Z 恢复，
# 并以数据文件（data.json）为客观依据断言。
#
# 前置：dev 应用已在运行且窗口可见（例如 `DOING_SHOW_ON_LAUNCH=1 DOING_SKIP_LOGIN=1 pnpm tauri:dev`）。
# 用法：scripts/e2e-keyboard.sh [进程名，默认 doing-desktop]
set -euo pipefail

PROC="${1:-doing-desktop}"
DATA="$HOME/Library/Application Support/dev.local.chmod777.Doing/data.json"

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "PASS: $*"; }

key() { osascript -e "tell application \"System Events\" to key code $1" >/dev/null; }
type_text() { osascript -e "tell application \"System Events\" to keystroke \"$1\"" >/dev/null; }
meta_key() { osascript -e "tell application \"System Events\" to keystroke \"$1\" using command down" >/dev/null; }

# 轮询数据文件直到条件满足（保存为同步写盘，正常 <1s）。
wait_data() {
  local tries=20
  while [ $tries -gt 0 ]; do
    if python3 - "$DATA" "$1" <<'EOF'
import json, os, sys
data = json.load(open(sys.argv[1]))
expr = sys.argv[2]
items = data["items"]
d0 = int(os.environ.get("DONE0", "0"))
# 支持的最小断言集合
if expr == "has:A": ok = any(i["text"] == "键盘冒烟-A" for i in items)
elif expr == "has:B": ok = any(i["text"] == "键盘冒烟-B" for i in items)
elif expr == "no_draft": ok = not any(i["text"] == "不应出现" for i in items)
elif expr == "done_increased": ok = sum(1 for i in items if i["done"]) >= d0 + 1
elif expr.startswith("done_back_to:"): ok = sum(1 for i in items if i["done"]) == int(expr.split(":")[1])
elif expr == "b_gone": ok = not any(i["text"] == "键盘冒烟-B" for i in items)
elif expr == "b_back": ok = any(i["text"] == "键盘冒烟-B" for i in items)
else: ok = False
sys.exit(0 if ok else 1)
EOF
    then return 0; fi
    sleep 0.5; tries=$((tries-1))
  done
  return 1
}
export DONE0=0

pgrep -f "$PROC" >/dev/null || fail "未找到运行中的进程：$PROC"
osascript -e "tell application \"System Events\" to set frontmost of process \"$PROC\" to true" >/dev/null
sleep 0.8
[ -f "$DATA" ] || fail "数据文件不存在：$DATA"
DONE0=$(python3 -c "import json;print(sum(1 for i in json.load(open('$DATA'))['items'] if i['done']))")
export DONE0

# 1) 录入：⌘N 聚焦 → 输入 → 回车
meta_key "n"; sleep 0.4
type_text "键盘冒烟-A"; sleep 0.4
key 36; sleep 0.6
wait_data "has:A" || fail "回车录入未落盘（键盘冒烟-A）"
ok "录入：回车提交并落盘（键盘冒烟-A）"

# 2) Escape 清草稿：输入 → Esc → 再输入本应只有后者存在
type_text "不应出现"; sleep 0.3
key 53; sleep 0.4
type_text "键盘冒烟-B"; sleep 0.3
key 36; sleep 0.6
wait_data "has:B" || fail "第二项录入失败（键盘冒烟-B）"
wait_data "no_draft" || fail "Escape 未清空草稿（不应出现 被打字提交）"
ok "Escape 先清空草稿（不应出现 未提交）"

# 3) ↑↓ 选择 + 空格完成：从录入框直接按下箭头，再空格
key 125; sleep 0.4
key 49; sleep 0.6
wait_data "done_increased" || fail "空格未完成选中事项"
ok "↑↓ 选择（起点在录入框）+ 空格完成生效"

# 4) ⌘Z 撤销完成
meta_key "z"; sleep 0.6
wait_data "done_back_to:$DONE0" || fail "⌘Z 未撤销完成"
ok "⌘Z 撤销完成"

# 5) 退格删除 + ⌘Z 恢复：连续下箭头到末尾（=冒烟-B），退格删除
for _ in 1 2 3 4 5 6; do key 125; sleep 0.15; done
key 51; sleep 0.6
wait_data "b_gone" || fail "⌫ 未删除末行（键盘冒烟-B）"
ok "⌫ 删除选中事项（键盘冒烟-B）"
meta_key "z"; sleep 0.6
wait_data "b_back" || fail "⌘Z 未恢复删除"
ok "⌘Z 恢复删除"

echo "== 真实 WebView 键盘交互记录全部通过 =="
