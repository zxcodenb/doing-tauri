import { act, cleanup, renderHook, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { ReactNode } from 'react'

const { invoke, listen, handlers } = vi.hoisted(() => ({
  invoke: vi.fn(), listen: vi.fn(), handlers: new Map<string, (event: { payload: unknown }) => void>(),
}))
vi.mock('@tauri-apps/api/core', () => ({ invoke }))
vi.mock('@tauri-apps/api/event', () => ({ listen }))

import { DoingProvider, useDoing } from './useDoing'
import { EVT } from '../lib/ipc'
import { conflictFixture, startupFixture } from '../test/fixtures'

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((done) => { resolve = done })
  return { promise, resolve }
}
function emit(name: string, payload: unknown) { act(() => handlers.get(name)?.({ payload })) }
function mount() { return renderHook(() => useDoing(), { wrapper: ({ children }: { children: ReactNode }) => <DoingProvider>{children}</DoingProvider> }) }

beforeEach(() => {
  listen.mockImplementation(async (name, handler) => { handlers.set(name, handler); return () => handlers.delete(name) })
  invoke.mockResolvedValue(startupFixture())
})
afterEach(() => { cleanup(); handlers.clear(); vi.resetAllMocks() })

describe('订阅握手与会话隔离', () => {
  it('必须等每个监听器注册完成后才能调用 init_state', async () => {
    const gate = deferred<() => void>()
    listen.mockImplementation((name, handler) => {
      handlers.set(name, handler)
      return name === EVT.syncState ? gate.promise : Promise.resolve(() => handlers.delete(name))
    })
    const { result } = mount()
    await act(async () => { await Promise.resolve() })
    expect(invoke).not.toHaveBeenCalled()
    await act(async () => gate.resolve(() => {}))
    await waitFor(() => expect(result.current.ready).toBe(true))
    expect(invoke).toHaveBeenCalledExactlyOnceWith('init_state')
  })

  it('迟到的启动握手不得把新账号、快照和同步状态回退', async () => {
    const gate = deferred<ReturnType<typeof startupFixture>>()
    invoke.mockReturnValue(gate.promise)
    const { result } = mount()
    await waitFor(() => expect(invoke).toHaveBeenCalled())
    const current = startupFixture(3, 20)
    current.auth.username = 'account-b'
    current.snapshot.revision = 9
    emit(EVT.authState, current.auth)
    emit(EVT.snapshot, current.snapshot)
    emit(EVT.syncState, { ...current.sync, state: 'synced' })
    await act(async () => gate.resolve(startupFixture(1, 1)))
    await waitFor(() => expect(result.current.ready).toBe(true))
    expect(result.current.auth?.username).toBe('account-b')
    expect(result.current.snapshot.revision).toBe(9)
    expect(result.current.sync?.state).toBe('synced')
  })

  it('先收到新会话快照时先隐藏旧账号；迟到的 A 登出/401/冲突不能覆盖 B', async () => {
    const { result } = mount()
    await waitFor(() => expect(result.current.ready).toBe(true))
    const accountB = startupFixture(4, 15)
    accountB.auth.username = 'account-b'
    emit(EVT.snapshot, accountB.snapshot)
    expect(result.current.auth).toBeNull()
    emit(EVT.authState, accountB.auth)
    emit(EVT.authState, { ...startupFixture(2, 100).auth, loggedIn: false })
    emit(EVT.sessionLost, { ...startupFixture(2, 101).auth, loggedIn: false, error: '旧账号失效' })
    emit(EVT.conflict, conflictFixture(1, 102))
    expect(result.current.auth?.loggedIn).toBe(true)
    expect(result.current.auth?.username).toBe('account-b')
    expect(result.current.conflict).toBeNull()
  })

  it('同会话也按事件修订丢弃旧 auth/sync，并清除已解决的冲突', async () => {
    const { result } = mount()
    await waitFor(() => expect(result.current.ready).toBe(true))
    emit(EVT.authState, { ...startupFixture(1, 8).auth, error: '当前错误' })
    emit(EVT.authState, startupFixture(1, 7).auth)
    expect(result.current.auth?.error).toBe('当前错误')
    const conflict = conflictFixture(1, 10)
    emit(EVT.conflict, conflict)
    expect(result.current.conflict?.candidateId).toBe(conflict.candidateId)
    emit(EVT.syncState, { ...startupFixture(1, 12).sync, state: 'synced' })
    expect(result.current.conflict).toBeNull()
    emit(EVT.conflict, conflict)
    emit(EVT.syncState, { ...startupFixture(1, 9).sync, state: 'conflict', conflictId: conflict.candidateId })
    expect(result.current.conflict).toBeNull()
    expect(result.current.sync?.state).toBe('synced')
  })

  it('同步状态换成另一个候选 ID 时不能继续显示旧确认面板', async () => {
    const { result } = mount()
    await waitFor(() => expect(result.current.ready).toBe(true))
    const first = conflictFixture()
    emit(EVT.conflict, first)
    const next = { ...conflictFixture(1, 3), candidateId: 'bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb' }
    emit(EVT.syncState, { ...startupFixture(1, 3).sync, state: 'conflict', conflictId: next.candidateId })
    expect(result.current.conflict).toBeNull()
    emit(EVT.conflict, first)
    expect(result.current.conflict).toBeNull()
    emit(EVT.conflict, next)
    expect(result.current.conflict?.candidateId).toBe(next.candidateId)
  })

  it('改变外观不应重新订阅或重新拉取启动状态', async () => {
    const { result } = mount()
    await waitFor(() => expect(result.current.ready).toBe(true))
    const count = listen.mock.calls.length
    emit(EVT.settings, { ...startupFixture().settings, eventRevision: 2, revision: 2, appearance: 'dark' })
    await waitFor(() => expect(document.documentElement.dataset.theme).toBe('dark'))
    expect(invoke).toHaveBeenCalledTimes(1)
    expect(listen).toHaveBeenCalledTimes(count)
  })

  it('卸载时清理已注册及迟到完成的订阅，且不继续握手', async () => {
    const gate = deferred<() => void>()
    const unlisten = vi.fn()
    listen.mockImplementation((name) => name === EVT.syncState ? gate.promise : Promise.resolve(unlisten))
    const { unmount } = mount()
    await act(async () => { await Promise.resolve() })
    const count = listen.mock.calls.length
    unmount()
    await act(async () => gate.resolve(unlisten))
    expect(unlisten).toHaveBeenCalledTimes(count)
    expect(invoke).not.toHaveBeenCalled()
  })
  it('监听器注册失败时不读取初始状态，错误可见且可重试', async () => {
    listen.mockImplementation(async (name, handler) => {
      if (name === EVT.syncState) throw new Error('订阅失败')
      handlers.set(name, handler)
      return () => handlers.delete(name)
    })
    const { result } = mount()
    await waitFor(() => expect(result.current.startupError).toBe('订阅失败'))
    expect(invoke).not.toHaveBeenCalled()
    expect(handlers.size).toBe(0)
    listen.mockImplementation(async (name, handler) => { handlers.set(name, handler); return () => handlers.delete(name) })
    act(() => result.current.retryStartup())
    await waitFor(() => expect(result.current.ready).toBe(true))
    expect(result.current.startupError).toBeNull()
  })

  it('结构化握手失败文案不会变成对象占位符，并能重试启动', async () => {
    invoke.mockRejectedValueOnce({ code: 'persistenceFailed', message: '无法读取本地数据', retryable: true, currentVersion: null })
    const { result } = mount()
    await waitFor(() => expect(result.current.startupError).toBe('无法读取本地数据'))
    act(() => result.current.retryStartup())
    await waitFor(() => expect(result.current.ready).toBe(true))
  })

  it('设置与迁移事件也按修订合并，旧握手不能回退它们', async () => {
    const gate = deferred<ReturnType<typeof startupFixture>>()
    invoke.mockReturnValue(gate.promise)
    const { result } = mount()
    await waitFor(() => expect(invoke).toHaveBeenCalled())
    emit(EVT.settings, { ...startupFixture(1, 20).settings, appearance: 'dark', revision: 2 })
    const migration = { eventRevision: 21, available: false, imported: true, detectedFile: null, error: null, backupPath: null, transactionId: null, recoveryRequired: false, preferencesAvailable: false, importedPreferences: false, requiresLogin: true, warnings: [] }
    emit(EVT.migration, migration)
    await act(async () => gate.resolve(startupFixture(1, 10)))
    expect(result.current.settings?.appearance).toBe('dark')
    expect(result.current.migration?.imported).toBe(true)
    emit(EVT.settings, startupFixture(1, 15).settings)
    emit(EVT.migration, { ...migration, eventRevision: 15, imported: false })
    expect(result.current.settings?.appearance).toBe('dark')
    expect(result.current.migration?.imported).toBe(true)
  })

  it('窗口唤起监听属于启动握手，已注册后才累计通知队列的唤醒信号', async () => {
    const { result } = mount()
    await waitFor(() => expect(result.current.ready).toBe(true))
    expect(result.current.windowShowRevision).toBe(0)
    emit(EVT.windowShown, undefined)
    expect(result.current.windowShowRevision).toBe(1)
  })

  it('新会话只有快照而 auth 事件缺失时重新拉取完整状态', async () => {
    const { result } = mount()
    await waitFor(() => expect(result.current.ready).toBe(true))
    const next = startupFixture(4, 20); next.auth.username = 'account-b'
    invoke.mockResolvedValue(next)
    emit(EVT.snapshot, next.snapshot)
    expect(result.current.auth).toBeNull()
    await waitFor(() => expect(result.current.auth?.username).toBe('account-b'))
    expect(invoke).toHaveBeenCalledTimes(2)
  })

})
