import { act, cleanup, renderHook, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { startupFixture } from '../test/fixtures'
import type { ScrollTargetView } from '../types'
const calls = vi.hoisted(() => ({ next: vi.fn(), ack: vi.fn() }))
vi.mock('../lib/ipc', () => ({ api: { notificationNext: calls.next, notificationAck: calls.ack } }))
vi.mock('./useDoing', () => ({ useDoing: () => state }))
import { useNotificationNavigation } from './useNotificationNavigation'

const fixture = () => startupFixture(3, 10)
let state: {
  ready: boolean; auth: ReturnType<typeof fixture>['auth']; snapshot: ReturnType<typeof fixture>['snapshot']; windowShowRevision: number;
  pushNotice: ReturnType<typeof vi.fn>; run: (callback: () => Promise<unknown>) => Promise<{ ok: boolean; message: string }>
}
const view = (overrides: Partial<ScrollTargetView> = {}): ScrollTargetView => ({
  notificationId: '99999999-9999-4999-8999-999999999999', itemId: fixture().snapshot.items[0].id,
  sessionGeneration: 3, eventRevision: 11, ...overrides,
})
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>((r) => { resolve = r }); return { resolve, promise } }
beforeEach(() => {
  const data = fixture()
  state = { ready: true, auth: data.auth, snapshot: data.snapshot, windowShowRevision: 0, pushNotice: vi.fn(),
    run: async (callback) => { try { await callback(); return { ok: true, message: '' } } catch { return { ok: false, message: 'failure' } } } }
  calls.next.mockResolvedValue(null); calls.ack.mockResolvedValue(undefined)
})
afterEach(() => { cleanup(); vi.resetAllMocks() })

describe('持久通知读取、会话与 ACK 时序', () => {
  it('等待启动握手及登录，不把未登录的读取当作消费', async () => {
    state.ready = false; state.auth.loggedIn = false
    calls.next.mockResolvedValue(view())
    const { result, rerender } = renderHook(useNotificationNavigation)
    expect(calls.next).not.toHaveBeenCalled()
    state.ready = true; rerender(); expect(calls.next).not.toHaveBeenCalled()
    state.auth = { ...state.auth, loggedIn: true }; rerender()
    await waitFor(() => expect(result.current.target?.notificationId).toBe(view().notificationId))
    expect(calls.ack).not.toHaveBeenCalled()
  })
  it('拒绝同 UUID 的旧会话读取结果', async () => {
    calls.next.mockResolvedValue(view({ sessionGeneration: 1, eventRevision: 999 }))
    const { result } = renderHook(useNotificationNavigation)
    await waitFor(() => expect(calls.next).toHaveBeenCalledTimes(1))
    expect(result.current.target).toBeNull(); expect(calls.ack).not.toHaveBeenCalled()
  })
  it('新窗口唤起读取已完成后，迟到的旧请求不能回退目标', async () => {
    const gate = deferred<ScrollTargetView | null>()
    const next = view({ notificationId: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', eventRevision: 12 })
    calls.next.mockReturnValueOnce(gate.promise).mockResolvedValueOnce(next)
    const { result, rerender } = renderHook(useNotificationNavigation)
    await waitFor(() => expect(calls.next).toHaveBeenCalledTimes(1))
    state.windowShowRevision++; rerender()
    await waitFor(() => expect(result.current.target?.notificationId).toBe(next.notificationId))
    await act(async () => gate.resolve(view()))
    expect(result.current.target?.notificationId).toBe(next.notificationId)
  })
  it('读取不会提前 ACK；并发确认只发一次，成功后读取下一个', async () => {
    const gate = deferred<void>()
    calls.next.mockResolvedValueOnce(view()).mockResolvedValue(null)
    calls.ack.mockReturnValue(gate.promise)
    const { result } = renderHook(useNotificationNavigation)
    await waitFor(() => expect(result.current.target).not.toBeNull())
    expect(calls.ack).not.toHaveBeenCalled()
    let first!: Promise<void>; let second!: Promise<void>
    act(() => { first = result.current.acknowledge(view()); second = result.current.acknowledge(view()) })
    expect(calls.ack).toHaveBeenCalledExactlyOnceWith(view().notificationId, 3)
    await act(async () => { gate.resolve(); await Promise.all([first, second]) })
    await waitFor(() => expect(result.current.target).toBeNull())
    expect(calls.next).toHaveBeenCalledTimes(2)
  })
  it('ACK 失败不清目标；再次唤起后可以重新确认', async () => {
    calls.next.mockResolvedValue(view()); calls.ack.mockRejectedValueOnce(new Error('disk full'))
    const { result, rerender } = renderHook(useNotificationNavigation)
    await waitFor(() => expect(result.current.target).not.toBeNull())
    await act(async () => result.current.acknowledge(view()))
    expect(result.current.target?.notificationId).toBe(view().notificationId)
    state.windowShowRevision++; rerender()
    await waitFor(() => expect(calls.next).toHaveBeenCalledTimes(2))
    calls.next.mockResolvedValue(null)
    await act(async () => result.current.acknowledge(view()))
    await waitFor(() => expect(result.current.target).toBeNull())
    expect(calls.ack).toHaveBeenCalledTimes(2)
  })
  it('读取失败可见，不确认无法读取的记录', async () => {
    calls.next.mockRejectedValue(new Error('损坏的通知记录'))
    const { result } = renderHook(useNotificationNavigation)
    await waitFor(() => expect(state.pushNotice).toHaveBeenCalledWith({ kind: 'error', message: '损坏的通知记录' }))
    expect(result.current.target).toBeNull(); expect(calls.ack).not.toHaveBeenCalled()
  })
})

it('旧会话 ACK 的迟到成功不得清除或重新读取新账号目标', async () => {
  const gate = deferred<void>()
  const next = view({ notificationId: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', sessionGeneration: 4, eventRevision: 22 })
  calls.next.mockResolvedValue(view()); calls.ack.mockReturnValue(gate.promise)
  const { result, rerender } = renderHook(useNotificationNavigation)
  await waitFor(() => expect(result.current.target).not.toBeNull())
  let ack!: Promise<void>
  act(() => { ack = result.current.acknowledge(view()) })
  state.auth = { ...state.auth, sessionGeneration: 4 }
  state.snapshot = { ...state.snapshot, sessionGeneration: 4, eventRevision: 20 }
  calls.next.mockResolvedValue(next); rerender()
  await waitFor(() => expect(result.current.target?.notificationId).toBe(next.notificationId))
  await act(async () => { gate.resolve(); await ack })
  expect(result.current.target?.notificationId).toBe(next.notificationId)
  expect(calls.next).toHaveBeenCalledTimes(2)
})
