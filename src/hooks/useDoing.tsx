// 全局状态 Provider：订阅 Rust 事件、暴露命令封装与 UI 反馈。

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import { api, bus, type Unlisten } from '../lib/ipc'
import { mergeSnapshot } from '../lib/snapshotMerge'
import type {
  AuthStateView,
  ConflictView,
  MigrationStatus,
  SettingsView,
  SnapshotView,
  SyncStatePayload,
} from '../types'

const EMPTY_SNAPSHOT: SnapshotView = {
  revision: 0,
  items: [],
  focusId: null,
  undoTitle: null,
  redoTitle: null,
  notifiedDueIds: [],
  saveFailed: false,
}

export interface NoticeState {
  id: number
  kind: 'info' | 'error'
  message: string
  offersUndo?: boolean
}

interface DoingState {
  ready: boolean
  snapshot: SnapshotView
  auth: AuthStateView | null
  settings: SettingsView | null
  sync: SyncStatePayload | null
  conflict: ConflictView | null
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

export function resolveTheme(settings: SettingsView | null): 'light' | 'dark' {
  const pref = settings?.appearance ?? 'system'
  if (pref !== 'system') return pref
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
}

export function DoingProvider({ children }: { children: ReactNode }) {
  const [ready, setReady] = useState(false)
  const [snapshot, setSnapshot] = useState<SnapshotView>(EMPTY_SNAPSHOT)
  const [auth, setAuth] = useState<AuthStateView | null>(null)
  const [settings, setSettings] = useState<SettingsView | null>(null)
  const [sync, setSync] = useState<SyncStatePayload | null>(null)
  const [conflict, setConflict] = useState<ConflictView | null>(null)
  const [migration, setMigration] = useState<MigrationStatus | null>(null)
  const [notices, setNotices] = useState<NoticeState[]>([])
  const [now, setNow] = useState(() => new Date())
  const noticeSeq = useRef(0)

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

  // 初始拉取与事件订阅（先订阅、后拉取：启动早期事件不丢，快照按 revision 防回退）。
  useEffect(() => {
    let disposed = false
    const unlistens: Unlisten[] = []
    const add = (p: Promise<Unlisten>) => {
      p.then((u) => {
        if (disposed) u()
        else unlistens.push(u)
      }).catch(() => {})
    }

    add(bus.snapshot((s) => setSnapshot((prev) => mergeSnapshot(prev, s))))
    add(bus.syncState(setSync))
    add(bus.authState(setAuth))
    add(bus.settings(setSettings))
    add(bus.conflict(setConflict))
    add(bus.migration(setMigration))
    add(bus.sessionLost(() => {
      setAuth((a) => (a ? { ...a, loggedIn: false } : a))
      setConflict(null)
      pushNotice({ kind: 'error', message: '登录状态已失效，请重新登录' })
    }))
    add(bus.saveFailed((message) => pushNotice({ kind: 'error', message })))

    api.initState().then((startup) => {
      if (disposed) return
      setAuth(startup.auth)
      setSnapshot((prev) => mergeSnapshot(prev, startup.snapshot))
      setSettings(startup.settings)
      setSync(startup.sync)
      setConflict((prev) => prev ?? startup.conflict)
      setMigration(startup.migration ?? (startup.legacyImportAvailable ? { available: true, detectedFile: null, imported: false, error: null, backupPath: null } : null))
      setReady(true)
    })

    // 主题：设置变化/系统变化即时生效（主面板、设置与弹层共享同一 DOM 根样式）。
    const applyTheme = () => {
      document.documentElement.dataset.theme = resolveTheme(settings)
    }
    applyTheme()
    const media = window.matchMedia('(prefers-color-scheme: dark)')
    const onMedia = () => applyTheme()
    media.addEventListener('change', onMedia)
    const timer = window.setInterval(() => setNow(new Date()), 30_000)

    return () => {
      disposed = true
      media.removeEventListener('change', onMedia)
      window.clearInterval(timer)
      unlistens.forEach((u) => u())
    }
  }, [settings?.appearance, pushNotice])

  const value = useMemo<DoingState>(
    () => ({
      ready,
      snapshot,
      auth,
      settings,
      sync,
      conflict,
      migration,
      notices,
      now,
      pushNotice,
      dismissNotice,
      run,
      undo: () => void api.historyUndo(),
      redo: () => void api.historyRedo(),
    }),
    [ready, snapshot, auth, settings, sync, conflict, migration, notices, now, pushNotice, dismissNotice, run],
  )

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>
}
