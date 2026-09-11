// 全局状态 Provider：订阅 Rust 事件、暴露命令封装与 UI 反馈。

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import { api, bus, type Unlisten } from '../lib/ipc'
import { EMPTY_REPLICA, mergeReplica } from '../lib/replica'
import type {
  AuthStateView,
  ConflictView,
  MigrationStatus,
  SettingsView,
  StartupView,
  SnapshotView,
  SyncStatePayload,
} from '../types'

export interface NoticeState {
  id: number
  kind: 'info' | 'error'
  message: string
  offersUndo?: boolean
}

interface DoingState {
  ready: boolean
  windowShowRevision: number
  startupError: string | null
  retryStartup: () => void
  snapshot: SnapshotView
  auth: AuthStateView | null
  settings: SettingsView | null
  sync: SyncStatePayload | null
  conflict: ConflictView | null
  conflictOpen: boolean
  showConflict: () => void
  deferConflict: () => Promise<void>
  migration: MigrationStatus | null
  notices: NoticeState[]
  now: Date
  pushNotice: (n: Omit<NoticeState, 'id'>) => void
  dismissNotice: (id: number) => void
  run: (fn: () => Promise<unknown>) => Promise<{ ok: boolean; message: string }>
  undo: () => void
  redo: () => void
}

const Ctx = createContext<DoingState | null>(null)

export function useDoing(): DoingState {
  const v = useContext(Ctx)
  if (!v) throw new Error('useDoing 必须在 DoingProvider 内使用')
  return v
}

export function resolveTheme(pref: SettingsView['appearance'] = 'system'): 'light' | 'dark' {
  if (pref !== 'system') return pref
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
}

export function DoingProvider({ children }: { children: ReactNode }) {
  const [ready, setReady] = useState(false)
  const [startupError, setStartupError] = useState<string | null>(null)
  const [startupAttempt, setStartupAttempt] = useState(0)
  const [replica, dispatch] = useReducer(mergeReplica, EMPTY_REPLICA)
  const { snapshot, auth, sync, conflict } = replica
  const [deferredConflict, setDeferredConflict] = useState<string | null>(null)
  const conflictOpen = conflict !== null && deferredConflict !== `${conflict.sessionGeneration}:${conflict.candidateId}`
  const showConflict = useCallback(() => setDeferredConflict(null), [])
  const retryStartup = useCallback(() => setStartupAttempt((attempt) => attempt + 1), [])
  const [settings, setSettings] = useState<SettingsView | null>(null)
  const [migration, setMigration] = useState<MigrationStatus | null>(null)
  const [notices, setNotices] = useState<NoticeState[]>([])
  const [now, setNow] = useState(() => new Date())
  const noticeSeq = useRef(0)
  const [windowShowRevision, setWindowShowRevision] = useState(0)

  const pushNotice = useCallback((n: Omit<NoticeState, 'id'>) => {
    noticeSeq.current += 1
    const id = noticeSeq.current
    setNotices((prev) => [...prev.slice(-2), { ...n, id }])
    window.setTimeout(() => {
      setNotices((prev) => prev.filter((x) => x.id !== id))
    }, 5000)
  }, [])

  const dismissNotice = useCallback((id: number) => {
    setNotices((prev) => prev.filter((x) => x.id !== id))
  }, [])

  const run = useCallback(
    async (fn: () => Promise<unknown>): Promise<{ ok: boolean; message: string }> => {
      try {
        const out = (await fn()) as { message?: string; offersUndo?: boolean } | null
        if (out?.message) {
          pushNotice({ kind: 'info', message: out.message, offersUndo: out.offersUndo })
        }
        return { ok: true, message: out?.message ?? '' }
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e)
        pushNotice({ kind: 'error', message })
        return { ok: false, message }
      }
    },
    [pushNotice],
  )

  const deferConflict = useCallback(async () => {
    if (!conflict) return
    const result = await run(() => api.conflictDefer())
    if (result.ok) setDeferredConflict(`${conflict.sessionGeneration}:${conflict.candidateId}`)
  }, [conflict, run])

  const applyStartup = useCallback((startup: StartupView) => {
    dispatch({ type: 'auth', payload: startup.auth })
    dispatch({ type: 'snapshot', payload: startup.snapshot })
    dispatch({ type: 'sync', payload: startup.sync })
    if (startup.conflict) dispatch({ type: 'conflict', payload: startup.conflict })
    setSettings((prev) => !prev || startup.settings.eventRevision > prev.eventRevision ? startup.settings : prev)
    const migration = startup.migration ?? (startup.legacyImportAvailable
      ? { eventRevision: startup.snapshot.eventRevision, available: true, detectedFile: null, imported: false, error: null, backupPath: null, transactionId: null, recoveryRequired: false, preferencesAvailable: false, importedPreferences: false, requiresLogin: false, warnings: [] } : null)
    if (migration) setMigration((prev) => !prev || migration.eventRevision > prev.eventRevision ? migration : prev)
  }, [])

  // 先完成所有订阅，再拉取初始状态；事件先于握手返回时也统一经过修订合并器。
  useEffect(() => {
    let disposed = false
    const unlistens: Unlisten[] = []
    const add = async (promise: Promise<Unlisten>) => {
      const unlisten = await promise
      if (disposed) unlisten()
      else unlistens.push(unlisten)
    }
    const active = <T,>(fn: (payload: T) => void) => (payload: T) => {
      if (!disposed) fn(payload)
    }
    const close = () => {
      disposed = true
      unlistens.splice(0).forEach((unlisten) => unlisten())
    }
    const initialize = async () => {
      setStartupError(null)
      try {
        await Promise.all([
          add(bus.windowShown(() => { if (!disposed) setWindowShowRevision((revision) => revision + 1) })),
          add(bus.snapshot(active((payload) => dispatch({ type: 'snapshot', payload })))),
          add(bus.syncState(active((payload) => dispatch({ type: 'sync', payload })))),
          add(bus.authState(active((payload) => dispatch({ type: 'auth', payload })))),
          add(bus.conflict(active((payload) => dispatch({ type: 'conflict', payload })))),
          add(bus.sessionLost(active((payload) => dispatch({ type: 'auth', payload })))),
          add(bus.settings(active((payload) => { setSettings((prev) => !prev || payload.eventRevision > prev.eventRevision ? payload : prev) }))),
          add(bus.migration(active((payload) => { setMigration((prev) => !prev || payload.eventRevision > prev.eventRevision ? payload : prev) }))),
          add(bus.saveFailed(active((message) => pushNotice({ kind: 'error', message })))),
        ])
        if (disposed) return
        const startup = await api.initState()
        if (disposed) return
        applyStartup(startup)
        setReady(true)
      } catch (error) {
        if (!disposed) {
          setStartupError(error instanceof Error ? error.message : String(error))
          close()
        }
      }
    }
    void initialize()
    return close
  }, [pushNotice, startupAttempt, applyStartup])

  // 如果会话事件有缺失（只收到新代次快照/同步状态），重新读取完整状态，而不是长期拼接旧账号。
  useEffect(() => {
    if (!ready || auth) return
    let disposed = false
    const timer = window.setTimeout(() => {
      void api.initState().then((startup) => { if (!disposed) applyStartup(startup) }).catch((error) => {
        if (!disposed) { setStartupError(error instanceof Error ? error.message : '无法恢复会话状态'); setReady(false) }
      })
    }, 75)
    return () => { disposed = true; window.clearTimeout(timer) }
  }, [ready, auth, replica.sessionGeneration, applyStartup])

  const appearance = settings?.appearance
  // 主题不是订阅的依赖：切换外观不能重新建立状态握手。
  useEffect(() => {
    const applyTheme = () => { document.documentElement.dataset.theme = resolveTheme(appearance) }
    applyTheme()
    const media = window.matchMedia('(prefers-color-scheme: dark)')
    media.addEventListener('change', applyTheme)
    return () => media.removeEventListener('change', applyTheme)
  }, [appearance])

  useEffect(() => {
    const timer = window.setInterval(() => setNow(new Date()), 30_000)
    return () => window.clearInterval(timer)
  }, [])

  useEffect(() => { setNotices([]) }, [replica.sessionGeneration])

  const value = useMemo<DoingState>(
    () => ({
      ready,
      windowShowRevision,
      startupError,
      retryStartup,
      snapshot,
      auth,
      settings,
      sync,
      conflict,
      conflictOpen,
      showConflict,
      deferConflict,
      migration,
      notices,
      now,
      pushNotice,
      dismissNotice,
      run,
      undo: () => void api.historyUndo(),
      redo: () => void api.historyRedo(),
    }),
    [ready, windowShowRevision, startupError, retryStartup, snapshot, auth, settings, sync, conflict, conflictOpen, showConflict, deferConflict, migration, notices, now, pushNotice, dismissNotice, run],
  )

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>
}
