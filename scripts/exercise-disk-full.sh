#!/usr/bin/env bash
# 只在新建的 1 MiB 临时卷演练 ENOSPC；不接收或覆盖已有数据目录。
# 覆盖原子替换和 no-clobber 新文件发布：主文件不损坏、导出不截断、无自身 tmp 残留。
# 需要 macOS hdiutil，不冒充 Windows 原生故障验收。
set -euo pipefail
cd "$(dirname "$0")/.."

WORK="$(mktemp -d)"
IMG="$WORK/doing-tiny.dmg"
MNT="$WORK/mnt"
ATTACH_ATTEMPTED=0
mkdir -p "$MNT"

cleanup() {
  local status=$?
  if [[ "$ATTACH_ATTEMPTED" == 1 ]]; then
    if ! hdiutil detach -quiet "$MNT"; then
      echo "临时卷未确认卸载；保留隔离目录，未执行目录删除：$WORK" >&2
      exit 1
    fi
  fi
  rm -rf -- "$WORK"
  echo "临时卷和目录已清理：$WORK"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

hdiutil create -quiet -size 1m -fs MS-DOS -volname DoingTest "$IMG"
ATTACH_ATTEMPTED=1
hdiutil attach -quiet -nobrowse -mountpoint "$MNT" "$IMG"
printf 'doing-core-disk-full-fixture-v1' > "$MNT/.doing-disk-full-fixture"

echo "小卷已挂载：$MNT ($(df -h "$MNT" | tail -1))"
DOING_SMALL_VOLUME="$MNT" cargo test -p doing-core --lib --locked disk_full -- --ignored --nocapture
