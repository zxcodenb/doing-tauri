// 快照合并守卫（计划 §3.4：事件与快照均带修订号，丢弃旧消息，必要时重新读取）。
import type { SnapshotView } from '../types'

/** revision 单调：仅当新快照修订号不落后时接受（同修订以最新收到为准）。 */
export function mergeSnapshot(prev: SnapshotView, next: SnapshotView): SnapshotView {
  return next.revision >= prev.revision ? next : prev
}
