// 顶栏：品牌区（拖动把手）、日期、钉住、更多菜单。

import { useState } from 'react'
import { api } from '../../lib/ipc'
import { useDoing } from '../../hooks/useDoing'
import { IconButton, Wordmark } from '../../components/ui'
import { requestFocus } from '../../lib/focusEvents'

const DRAG_ATTR = 'data-tauri-drag-region' as const

export function ChromeBar({ panelMode }: { panelMode: boolean }) {
  const { snapshot, run } = useDoing()
  const [menuOpen, setMenuOpen] = useState(false)

  const today = new Date()
  const dateLabel = `${today.getMonth() + 1}/${today.getDate()} ${['周日', '周一', '周二', '周三', '周四', '周五', '周六'][today.getDay()]}`

  const pin = async () => {
    await api.systemToggleMode()
  }

  const menuItems: { label: string; action: () => void; disabled?: boolean; danger?: boolean }[] = [
    { label: '新增事项', action: () => requestFocus() },
    { label: '设置…', action: () => void api.systemOpenSettings() },
    { label: 'separator' as string, action: () => {} },
    { label: `撤销${snapshot.undoTitle ?? ''}`, disabled: !snapshot.undoTitle, action: () => void run(() => api.historyUndo()) },
    { label: `重做${snapshot.redoTitle ?? ''}`, disabled: !snapshot.redoTitle, action: () => void run(() => api.historyRedo()) },
    { label: 'separator' as string, action: () => {} },
    { label: '清除已完成', disabled: !snapshot.items.some((i) => i.done), action: () => void run(() => api.taskClearCompleted()) },
    { label: '收起面板', action: () => void api.systemHideMain() },
    { label: '退出 Doing', danger: true, action: () => void api.systemQuit() },
  ]

  return (
    <div
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 6,
        padding: '14px 20px 8px',
        position: 'relative',
      }}
    >
      <div {...{ [DRAG_ATTR]: true }} style={{ display: 'inline-flex', alignItems: 'center' }}>
        <Wordmark />
      </div>
      <span style={{ flex: 1 }} />
      {snapshot ? (
        <span style={{ fontSize: 11, color: 'var(--text-dim)', fontWeight: 500 }}>{dateLabel}</span>
      ) : null}
      <IconButton
        icon={panelMode ? 'pinFill' : 'pin'}
        help={panelMode ? '收回菜单栏' : '钉在桌面上'}
        active={panelMode}
        onAction={() => void pin()}
      />
      <IconButton icon="dots" help="更多操作" onAction={() => setMenuOpen((v) => !v)} />
      {menuOpen ? (
        <div
          style={{
            position: 'absolute',
            right: 12,
            top: 38,
            zIndex: 80,
            minWidth: 200,
            background: 'var(--bg)',
            border: '1px solid var(--stroke)',
            borderRadius: 12,
            boxShadow: 'var(--shadow-panel)',
            padding: 5,
          }}
        >
          {menuItems.map((item, i) =>
            item.label === 'separator' ? (
              <div key={`s${i}`} style={{ height: 1, background: 'var(--divider)', margin: '4px 6px' }} />
            ) : (
              <button
                key={item.label}
                type="button"
                disabled={item.disabled}
                onClick={() => {
                  setMenuOpen(false)
                  item.action()
                }}
                style={{
                  display: 'flex',
                  width: '100%',
                  padding: '8px 10px',
                  borderRadius: 8,
                  fontSize: 13,
                  color: item.danger ? 'var(--danger)' : 'var(--text)',
                  opacity: item.disabled ? 0.4 : 1,
                  textAlign: 'left',
                }}
                onMouseEnter={(e) => (e.currentTarget.style.background = 'var(--surface-raised)')}
                onMouseLeave={(e) => (e.currentTarget.style.background = 'transparent')}
              >
                {item.label}
              </button>
            ),
          )}
        </div>
      ) : null}
    </div>
  )
}
