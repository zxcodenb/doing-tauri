// Rust 是唯一写入方。不同事件通道按各自修订合并，但共享会话水位，不能拼接 A/B 两个会话。
import type { AuthStateView, ConflictView, SnapshotView, SyncStatePayload } from '../types'
import { mergeSnapshot } from './snapshotMerge'

export const EMPTY_SNAPSHOT: SnapshotView = {
  sessionGeneration: 0, eventRevision: 0, revision: 0,
  items: [], focusId: null, undoTitle: null, redoTitle: null, notifiedDueIds: [], saveFailed: false,
}

export interface Replica {
  sessionGeneration: number
  snapshot: SnapshotView
  auth: AuthStateView | null
  sync: SyncStatePayload | null
  conflict: ConflictView | null
  // 冲突被清除后仍保留水位，防止迟到的候选重新打开已解决的面板。
  conflictRevision: number
}
export const EMPTY_REPLICA: Replica = {
  sessionGeneration: 0, snapshot: EMPTY_SNAPSHOT, auth: null, sync: null, conflict: null, conflictRevision: 0,
}
export type ReplicaEvent =
  | { type: 'snapshot'; payload: SnapshotView }
  | { type: 'auth'; payload: AuthStateView }
  | { type: 'sync'; payload: SyncStatePayload }
  | { type: 'conflict'; payload: ConflictView }

export function mergeReplica(current: Replica, event: ReplicaEvent): Replica {
  const { sessionGeneration, eventRevision } = event.payload
  if (sessionGeneration < current.sessionGeneration) return current
  const state = sessionGeneration > current.sessionGeneration
    ? { ...EMPTY_REPLICA, sessionGeneration, snapshot: { ...EMPTY_SNAPSHOT, sessionGeneration } }
    : current
  switch (event.type) {
    case 'snapshot': {
      const snapshot = mergeSnapshot(state.snapshot, event.payload)
      return snapshot === state.snapshot ? state : { ...state, snapshot }
    }
    case 'auth':
      if (state.auth && eventRevision <= state.auth.eventRevision) return state
      return { ...state, auth: event.payload }
    case 'sync': {
      if (state.sync && eventRevision <= state.sync.eventRevision) return state
      const clearConflict = eventRevision >= state.conflictRevision
        && (event.payload.state !== 'conflict' || state.conflict?.candidateId !== event.payload.conflictId)
      return {
        ...state, sync: event.payload,
        conflict: clearConflict ? null : state.conflict,
        conflictRevision: Math.max(eventRevision, state.conflictRevision),
      }
    }
    case 'conflict':
      if (state.auth?.loggedIn === false || eventRevision < state.conflictRevision) return state
      if (eventRevision === state.sync?.eventRevision
        && (state.sync.state !== 'conflict' || state.sync.conflictId !== event.payload.candidateId)) return state
      return { ...state, conflict: event.payload, conflictRevision: eventRevision }
  }
}
