// 同会话的数据修订和事件修订均不得回退；新的账号会话不能被旧账号的大 revision 阻塞。
import type { SnapshotView } from '../types'

export function mergeSnapshot(prev: SnapshotView, next: SnapshotView): SnapshotView {
  if (next.sessionGeneration !== prev.sessionGeneration) {
    return next.sessionGeneration > prev.sessionGeneration ? next : prev
  }
  return next.eventRevision > prev.eventRevision && next.revision >= prev.revision ? next : prev
}
