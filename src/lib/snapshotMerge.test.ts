import { describe, expect, it } from 'vitest'
import { mergeSnapshot } from './snapshotMerge'
import type { SnapshotView } from '../types'

function snap(revision: number, text: string): SnapshotView {
  return {
    revision,
    items: [
      {
        id: '11111111-1111-1111-1111-111111111111',
        text,
        done: false,
        createdAt: '2026-09-10T00:00:00Z',
        dueDate: null,
        updatedAt: '2026-09-10T00:00:00Z',
      },
    ],
    focusId: null,
    undoTitle: null,
    redoTitle: null,
    notifiedDueIds: [],
    saveFailed: false,
  }
}

describe('mergeSnapshot（旧修订必须被丢弃）', () => {
  it('新修订更大：接受', () => {
    const merged = mergeSnapshot(snap(3, 'old'), snap(4, 'new'))
    expect(merged.revision).toBe(4)
    expect(merged.items[0].text).toBe('new')
  })
  it('新修订更小（迟到的事件）：丢弃', () => {
    const merged = mergeSnapshot(snap(9, 'current'), snap(6, 'stale'))
    expect(merged.revision).toBe(9)
    expect(merged.items[0].text).toBe('current')
  })
  it('同修订：以最新收到为准（允许服务端同修订重发覆盖）', () => {
    const merged = mergeSnapshot(snap(5, 'a'), snap(5, 'b'))
    expect(merged.items[0].text).toBe('b')
  })
})
