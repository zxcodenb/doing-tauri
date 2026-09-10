// 应用壳：窗口识别、全局快捷键、失焦收起、通知点击、迁移提示。

import { useEffect, useRef, useState } from 'react'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { onAction } from '@tauri-apps/plugin-notification'
import { DoingProvider, useDoing } from './hooks/useDoing'
import { api, bus } from './lib/ipc'
import { LoginView } from './features/auth/LoginView'
import { Workspace } from './features/workspace/Workspace'
import { ConflictOverlay } from './features/workspace/ConflictOverlay'
import { SettingsApp } from './features/settings/SettingsApp'
import { ActionButton } from './components/ui'
import { Icon } from './components/icons'

export function App() {
  return (
    <DoingProvider>
      <Shell />
    </DoingProvider>
  )
}

function Shell() {
  const { ready, auth, settings, conflict, migration, run } = useDoing()
  const composing = useRef(false)
  const shownAt = useRef(0)
  const [windowLabel, setWindowLabel] = useState('main')
  const [migrating, setMigrating] = useState(false)

  useEffect(() => {
    const label = getCurrentWindow().label
    setWindowLabel(label)
    document.documentElement.dataset.win = label === 'settings' ? 'settings' : 'main'
  }, [])
  // 全局快捷键；IME 组合期间不误触。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.isComposing) return
      composing.current = e.isComposing
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
          void api.systemHideMain()
          break
        case 'q':
          if (!e.shiftKey) {
            e.preventDefault()
            void api.systemQuit()
          }
          break
      }
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [])

  // 失焦收起：应用退到后台时隐藏菜单栏形态（组合输入豁免；主动唤起后 1.2s 内豁免）。
  useEffect(() => {
    if (windowLabel !== 'main') return
    let unlisten: (() => void) | undefined
    let unShown: (() => void) | undefined
    void bus
      .windowShown(() => {
        shownAt.current = Date.now()
      })
      .then((u) => {
        unShown = u
      })
    void getCurrentWindow()
      .onFocusChanged(({ payload }) => {
        if (payload) {
          window.dispatchEvent(new Event('doing://window-focused'))
          return
        }
        window.setTimeout(() => {
          if (Date.now() - shownAt.current < 1200) return // 刚被主动唤起，避免抢焦点失败即收起
          if (document.visibilityState === 'hidden' && !composing.current) {
            void api.systemHideMain()
          }
        }, 120)
      })
      .then((u) => {
        unlisten = u
      })
    return () => {
      unlisten?.()
      unShown?.()
    }
  }, [windowLabel])

  // 通知点击：定位任务（数字 id 由 Rust 映射）或打开面板。
  useEffect(() => {
    let unlisten: (() => void) | undefined
    void onAction((notification) => {
      if (notification.id != null) void api.systemNotifyClicked(notification.id)
      else void api.systemOpenSettings()
    }).then((u) => {
      unlisten = u as unknown as () => void
    })
    return () => unlisten?.()
  }, [])

  // 托盘「从云端恢复…」等入口：打开设置窗口并定位到指定板块。
  useEffect(() => {
    let unlisten: (() => void) | undefined
    void bus
      .openSettings((section) => {
        try {
          localStorage.setItem('doing.openSettingsSection', section)
        } catch {
          /* 忽略存储异常 */
        }
        void api.systemOpenSettings()
      })
      .then((u) => {
        unlisten = u
      })
    return () => unlisten?.()
  }, [])

  if (!ready) {
    return (
      <div style={{ height: '100%', display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
        <span style={{ fontSize: 12, color: 'var(--text-dim)' }}>Doing 启动中…</span>
      </div>
    )
  }

  if (windowLabel === 'settings') {
    return <SettingsApp />
  }

  const panelMode = settings?.mode === 'panel'

  return (
    <div style={{ height: '100%', position: 'relative' }}>
      {auth?.loggedIn ? (
        <Workspace panelMode={panelMode} />
      ) : (
        <div style={{ height: '100%', display: 'flex', flexDirection: 'column' }}>
          <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
            <LoginView />
          </div>
        </div>
      )}
      {conflict ? <ConflictOverlay /> : null}
      {migration?.available && !migration.imported ? (
        <MigrationBanner
          busy={migrating}
          onImport={() => {
            const path = migration.detectedFile
            if (!path) return
            setMigrating(true)
            void run(() => importMigration(path)).finally(() => setMigrating(false))
          }}
        />
      ) : null}
    </div>
  )
}

async function importMigration(path: string): Promise<unknown> {
  return api.migrationImport(path)
}

function MigrationBanner({ busy, onImport }: { busy: boolean; onImport: () => void }) {
  return (
    <div
      style={{
        position: 'absolute',
        left: 16,
        right: 16,
        bottom: 86,
        background: 'var(--surface)',
        border: '1px solid var(--violet)',
        borderRadius: 12,
        padding: '10px 12px',
        display: 'flex',
        alignItems: 'center',
        gap: 10,
        boxShadow: 'var(--shadow-panel)',
        zIndex: 90,
      }}
    >
      <Icon name="link" size={14} />
      <span style={{ flex: 1, fontSize: 12, lineHeight: 1.5 }}>
        检测到旧版 Doing 数据，是否导入此设备？导入前会自动备份，原文件不会被修改。
      </span>
      <ActionButton kind="primary" disabled={busy} onClick={onImport}>
        {busy ? '导入中…' : '导入'}
      </ActionButton>
    </div>
  )
}
