import { describe, expect, it } from 'vitest'
import { mergeSnapshot } from './snapshotMerge'
import type { SnapshotView } from '../types'

function snap(revision: number, text: string, eventRevision = revision, sessionGeneration = 1): SnapshotView {
  return {
    revision,
    eventRevision,
    sessionGeneration,
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
  it('同数据修订：仅接收更大的事件修订（例如保存失败变化）', () => {
    const merged = mergeSnapshot(snap(5, 'a', 5), snap(5, 'b', 6))
    expect(merged.items[0].text).toBe('b')
  })
  it('同数据修订的迟到事件不能回退保存状态', () => {
    const current = { ...snap(5, 'current', 8), saveFailed: true }
    expect(mergeSnapshot(current, snap(5, 'stale', 7))).toBe(current)
    expect(mergeSnapshot(current, snap(5, 'duplicate', 8))).toBe(current)
  })
  it('新会话优先于旧账号的数据修订，不能残留旧账号事项', () => {
    expect(mergeSnapshot(snap(100, 'account-a', 100, 1), snap(2, 'account-b', 101, 3)).items[0].text).toBe('account-b')
  })
  it('旧会话即使带较大修订也不能越权替换当前快照', () => {
    const current = snap(2, 'account-b', 101, 3)
    expect(mergeSnapshot(current, snap(1000, 'account-a', 1000, 1))).toBe(current)
  })
})
