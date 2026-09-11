// 应用壳：窗口识别、全局快捷键、失焦收起、通知点击、迁移提示。

import { useEffect, useRef, useState } from 'react'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { DoingProvider, useDoing } from './hooks/useDoing'
import { api, bus } from './lib/ipc'
import { LoginView } from './features/auth/LoginView'
import { Workspace } from './features/workspace/Workspace'
import { ConflictOverlay } from './features/workspace/ConflictOverlay'
import { SettingsApp } from './features/settings/SettingsApp'
import { ActionButton } from './components/ui'
import { MigrationPanel } from './features/migration/MigrationPanel'

export function App() {
  return (
    <DoingProvider>
      <Shell />
    </DoingProvider>
  )
}

function Shell() {
  const { ready, startupError, retryStartup, auth, settings, conflict, migration, run } = useDoing()
  const composing = useRef(false)
  const shownAt = useRef(0)
  const [windowLabel] = useState(() => getCurrentWindow().label)

  useEffect(() => {
    document.documentElement.dataset.win = windowLabel === 'settings' ? 'settings' : 'main'
  }, [windowLabel])
  // 全局快捷键；IME 组合期间不误触。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.isComposing || composing.current) return
      const meta = e.metaKey || e.ctrlKey
      if (!meta || e.altKey) return
      const target = e.target as HTMLElement
      const inText =
        target.tagName === 'TEXTAREA' || target.tagName === 'INPUT' || target.isContentEditable
      const key = e.key.toLowerCase()
      if (key === 'z' && inText) return // 保留编辑器原生撤销
      switch (key) {
        case 'n':
          e.preventDefault()
          window.dispatchEvent(new Event('doing://focus-request'))
          break
        case ',':
          e.preventDefault()
          void api.systemOpenSettings()
          break
        case 'z':
          e.preventDefault()
          if (e.shiftKey) void api.historyRedo()
          else void api.historyUndo()
          break
        case 'w':
          e.preventDefault()
          if (windowLabel === 'settings') void run(() => getCurrentWindow().close())
          else void run(() => api.systemHideMain())
          break
        case 'q':
          if (!e.shiftKey) {
            e.preventDefault()
            void api.systemQuit()
          }
          break
      }
    }
    const startComposition = () => { composing.current = true }
    const endComposition = () => { composing.current = false }
    document.addEventListener('keydown', onKey)
    document.addEventListener('compositionstart', startComposition)
    document.addEventListener('compositionend', endComposition)
    return () => {
      document.removeEventListener('keydown', onKey)
      document.removeEventListener('compositionstart', startComposition)
      document.removeEventListener('compositionend', endComposition)
    }
  }, [windowLabel, run])

  // 只收起临时弹窗；IME/弹层豁免，重新获焦或卸载会取消延迟检查。
  useEffect(() => {
    if (windowLabel !== 'main' || settings?.mode !== 'popover') return
    let disposed = false
    let focused = true
    let timer: number | undefined
    const unlistens: (() => void)[] = []
    const add = (promise: Promise<() => void>) => {
      void promise.then((unlisten) => { if (disposed) unlisten(); else unlistens.push(unlisten) }).catch(() => {})
    }
    add(bus.windowShown(() => { if (!disposed) shownAt.current = Date.now() }))
    add(getCurrentWindow().onFocusChanged(({ payload }) => {
      if (disposed) return
      focused = payload
      window.clearTimeout(timer)
      if (payload) { window.dispatchEvent(new Event('doing://window-focused')); return }
      timer = window.setTimeout(() => {
        const editingLayer = document.querySelector('[role="dialog"], [role="menu"]')
        if (!disposed && !focused && !composing.current && !editingLayer && Date.now() - shownAt.current >= 1200) {
          void run(() => api.systemHideMain())
        }
      }, 120)
    }))
    return () => { disposed = true; window.clearTimeout(timer); unlistens.forEach((unlisten) => unlisten()) }
  }, [windowLabel, settings?.mode, run])

  // 托盘入口由主窗口负责唤起设置，避免两窗口重复处理同一个事件。
  useEffect(() => {
    if (windowLabel !== 'main') return
    let disposed = false
    let unlisten: (() => void) | undefined
    void bus.openSettings((section) => {
      if (disposed) return
      try { localStorage.setItem('doing.openSettingsSection', section) } catch { /* 无持久化仍可打开设置 */ }
      void run(() => api.systemOpenSettings())
    }).then((value) => { if (disposed) value(); else unlisten = value }).catch(() => {})
    return () => { disposed = true; unlisten?.() }
  }, [windowLabel, run])

  if (!ready) {
    return (
      <div style={{ height: '100%', display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
        {startupError ? <div role="alert"><p>启动失败：{startupError}</p><ActionButton onClick={retryStartup}>重试</ActionButton></div> : <span style={{ fontSize: 12, color: 'var(--text-dim)' }}>Doing 启动中…</span>}
      </div>
    )
  }

  if (windowLabel === 'settings') {
    return (
      <div style={{ height: '100%', position: 'relative' }}>
        <SettingsApp />
        {migration?.recoveryRequired ? <MigrationPanel key={migration.transactionId ?? 'migration'} status={migration} /> : null}
      </div>
    )
  }

  const panelMode = settings?.mode === 'panel'

  return (
    <div style={{ height: '100%', position: 'relative' }}>
      {auth?.loggedIn ? (
        <Workspace key={auth.sessionGeneration} panelMode={panelMode} />
      ) : (
        <div style={{ height: '100%', display: 'flex', flexDirection: 'column' }}>
          <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
            <LoginView />
          </div>
        </div>
      )}
      {auth?.loggedIn && conflict ? <ConflictOverlay key={conflict.candidateId} /> : null}
      {migration ? <MigrationPanel key={migration.transactionId ?? migration.detectedFile ?? 'migration'} status={migration} /> : null}
    </div>
  )
}
