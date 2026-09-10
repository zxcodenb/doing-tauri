#!/usr/bin/env bash
# 磁盘满（ENOSPC）故障演练（计划 §7.2 故障矩阵：磁盘满/无写权限/任意步骤中断）。
# 在 1MB 小容量卷上运行 repo 的原子提交：验证磁盘满时提交失败、
# 已提交数据不损坏、失败提交不残留自身 tmp。
# 需要 macOS（hdiutil）；不作为 CI 必跑项，Ubuntu 侧由只读目录自动化测试覆盖同类路径。
set -euo pipefail
cd "$(dirname "$0")/.."

WORK="$(mktemp -d)"
IMG="$WORK/doing-tiny.dmg"
MNT="$WORK/mnt"
mkdir -p "$MNT"

cleanup() {
  hdiutil detach -quiet "$MNT" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

hdiutil create -quiet -size 1m -fs MS-DOS -volname DoingTest "$IMG"
hdiutil attach -quiet -nobrowse -mountpoint "$MNT" "$IMG"

echo "小卷已挂载：$MNT ($(df -h "$MNT" | tail -1))"
DOING_SMALL_VOLUME="$MNT" cargo test -p doing-core --lib disk_full -- --ignored --nocapture
