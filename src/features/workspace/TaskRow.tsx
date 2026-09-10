// 任务行组件：完成按钮、文本、截止 chip、内联编辑与行菜单。
// 行级选中与键盘导航由 Workspace 统一管理。

import { api } from '../../lib/ipc'
import { useDoing } from '../../hooks/useDoing'
import { size } from '../../theme/tokens'
import type { ItemView, SettingsView } from '../../types'
import { Icon } from '../../components/icons'
import { DueChip, DuePicker } from '../../components/DuePicker'
import { useState } from 'react'

export function CompletionButton({
  item,
  isFocus = false,
  disabled = false,
}: {
  item: ItemView
  isFocus?: boolean
  disabled?: boolean
}) {
  const { run } = useDoing()
  const stroke = item.done ? 'var(--violet)' : isFocus ? 'var(--focus-stroke)' : 'var(--marker)'
  return (
    <button
      type="button"
      aria-label={`${item.done ? '恢复为待办' : '完成'}：${item.text}`}
      title={item.done ? '恢复为待办' : '标记完成'}
      disabled={disabled}
      onClick={() => void run(() => api.taskToggleDone(item.id))}
      style={{
        width: 28,
        height: 30,
        display: 'inline-flex',
        alignItems: 'flex-start',
        justifyContent: 'center',
        paddingTop: 4,
        opacity: disabled ? 0.5 : 1,
        flex: 'none',
      }}
    >
      <span
        style={{
          width: size.markerSize,
          height: size.markerSize,
          borderRadius: 5,
          border: `1.5px solid ${stroke}`,
          background: item.done ? 'var(--violet)' : 'transparent',
          color: 'var(--bg)',
          display: 'inline-flex',
          alignItems: 'center',
          justifyContent: 'center',
        }}
      >
        {item.done ? <Icon name="check" size={10} strokeWidth={3.2} /> : null}
      </span>
    </button>
  )
}

/** 行内编辑器的提交由 ⌘↩ / 按钮触发（IME 组合期间 Enter 不提交）。 */
export function InlineEditor({
  initial,
  isFocus,
  onSave,
  onCancel,
}: {
  initial: string
  isFocus: boolean
  onSave: (text: string) => void
  onCancel: () => void
}) {
  const [text, setText] = useState(initial)
  const [composing, setComposing] = useState(false)
  const canSave = text.trim().length > 0

  const commit = () => {
    if (canSave) onSave(text.trim())
  }

  return (
    <div>
      <textarea
        value={text}
        autoFocus
        onChange={(e) => setText(e.target.value)}
        onCompositionStart={() => setComposing(true)}
        onCompositionEnd={() => setComposing(false)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && !e.shiftKey && !composing && !e.metaKey && !e.ctrlKey) {
            e.preventDefault()
            commit()
          } else if (e.key === 'Escape') {
            e.preventDefault()
            onCancel()
          } else if (e.key === 'Enter' && e.metaKey) {
            e.preventDefault()
            commit()
          }
        }}
        placeholder="事项内容"
        rows={Math.min(6, Math.max(1, text.split('\n').length))}
        style={{
          width: '100%',
          resize: 'none',
          outline: 'none',
          background: isFocus ? 'var(--surface)' : 'var(--surface-raised)',
          borderRadius: 7,
          padding: 8,
          fontSize: isFocus ? 18 : 14,
          color: 'var(--text)',
          fontFamily: 'inherit',
          lineHeight: 1.4,
        }}
      />
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginTop: 8 }}>
        <span style={{ fontSize: 11, color: isFocus ? 'var(--focus-dim)' : 'var(--text-dim)' }}>
          ⌘↩ 保存
        </span>
        <span style={{ flex: 1 }} />
        <button
          type="button"
          onClick={onCancel}
          style={{ fontSize: 12, fontWeight: 600, color: 'var(--text)' }}
        >
          取消
        </button>
        <button
          type="button"
          onClick={commit}
          disabled={!canSave}
          style={{
            fontSize: 12,
            fontWeight: 600,
            color: 'var(--on-accent)',
            background: 'var(--accent)',
            borderRadius: 10,
            padding: '8px 13px',
            opacity: canSave ? 1 : 0.45,
          }}
        >
          保存
        </button>
      </div>
    </div>
  )
}

export function TaskRow({
  item,
  isFocus = false,
  selected,
  editing,
  beginEdit,
  endEdit,
  now,
  settings,
  onSelect,
}: {
  item: ItemView
  isFocus?: boolean
  selected: boolean
  editing: boolean
  beginEdit: () => void
  endEdit: () => void
  now: Date
  settings: SettingsView
  onSelect: () => void
}) {
  const { run } = useDoing()
  const [hovering, setHovering] = useState(false)
  const [dueOpen, setDueOpen] = useState(false)
  const [menuOpen, setMenuOpen] = useState(false)

  const saveEdit = (text: string) => {
    void run(() => api.taskEdit(item.id, text))
    endEdit()
  }

  const rowActions: { label: string; icon: 'pencil' | 'scope' | 'calendar' | 'check' | 'trash'; action: () => void; danger?: boolean }[] = [
    { label: '编辑事项', icon: 'pencil', action: beginEdit },
    ...(!item.done
      ? ([
          {
            label: isFocus ? '取消当前焦点' : '设为当前焦点',
            icon: 'scope' as const,
            action: () => void run(() => api.taskToggleFocus(item.id)),
          },
          {
            label: item.dueDate ? '修改截止时间…' : '设置截止时间…',
            icon: 'calendar' as const,
            action: () => {
              setMenuOpen(false)
              setDueOpen(true)
            },
          },
        ] as const)
      : []),
    {
      label: item.done ? '恢复为待办' : '标记完成',
      icon: 'check',
      action: () => void run(() => api.taskToggleDone(item.id)),
    },
    {
      label: '删除事项',
      icon: 'trash',
      danger: true,
      action: () => void run(() => api.taskDelete(item.id)),
    },
  ]

  return (
    <div style={{ position: 'relative' }}>
      <div
        onMouseEnter={() => setHovering(true)}
        onMouseLeave={() => setHovering(false)}
        onClick={(e) => {
          // 编辑器/输入控件内的点击不触发行选中（否则会抢走输入焦点）。
          const tag = (e.target as HTMLElement).tagName
          if (tag === 'TEXTAREA' || tag === 'INPUT' || tag === 'SELECT') return
          onSelect()
        }}
        onDoubleClick={(e) => {
          e.preventDefault()
          beginEdit()
        }}
        style={{
          display: 'flex',
          gap: 6,
          alignItems: 'flex-start',
          padding: editing ? '2px 6px' : '6px 6px',
          borderRadius: size.rowRadius,
          background: !isFocus && (hovering || editing) ? 'var(--surface)' : 'transparent',
          outline:
            selected && !editing
              ? `1px solid ${isFocus ? 'var(--focus-stroke)' : 'rgba(117,68,168,0.55)'}`
              : 'none',
          cursor: editing ? 'auto' : 'default',
        }}
      >
        <CompletionButton item={item} isFocus={isFocus} disabled={editing} />
        <div style={{ flex: 1, minWidth: 0 }}>
          {editing ? (
            <InlineEditor
              initial={item.text}
              isFocus={isFocus}
              onSave={saveEdit}
              onCancel={endEdit}
            />
          ) : (
            <div style={{ padding: '3px 0' }}>
              <div
                style={{
                  fontSize: isFocus ? 18 : 14,
                  fontWeight: isFocus ? 600 : 400,
                  color: item.done
                    ? 'var(--text-dim)'
                    : isFocus
                      ? 'var(--focus-text)'
                      : 'var(--text)',
                  textDecoration: item.done ? 'line-through' : undefined,
                  textDecorationColor: 'var(--text-faint)',
                  lineHeight: 1.4,
                  display: '-webkit-box',
                  WebkitLineClamp: 3,
                  WebkitBoxOrient: 'vertical',
                  overflow: 'hidden',
                  wordBreak: 'break-word',
                  whiteSpace: 'pre-wrap',
                }}
              >
                {item.text}
              </div>
              {item.dueDate && !item.done ? (
                <DueChip
                  due={item.dueDate}
                  now={now}
                  dueSoonEnabled={settings.dueSoonEnabled}
                  dueSoonHours={settings.dueSoonHours}
                  isFocus={isFocus}
                  onClick={(e) => {
                    e.stopPropagation()
                    setDueOpen(true)
                  }}
                />
              ) : null}
            </div>
          )}
        </div>
        {!editing ? (
          <button
            type="button"
            aria-label={`事项操作：${item.text}`}
            title="编辑、截止时间与更多操作"
            onClick={(e) => {
              e.stopPropagation()
              setMenuOpen((v) => !v)
            }}
            style={{
              width: 28,
              height: 30,
              display: 'inline-flex',
              alignItems: 'center',
              justifyContent: 'center',
              color: isFocus ? 'var(--focus-dim)' : 'var(--text-faint)',
              borderRadius: 7,
              background: menuOpen ? 'var(--surface-raised)' : 'transparent',
              flex: 'none',
            }}
          >
            <Icon name="dots" size={13} />
          </button>
        ) : null}
      </div>

      {menuOpen ? (
        <div
          style={{
            position: 'absolute',
            right: 8,
            top: '100%',
            zIndex: 50,
            background: 'var(--bg)',
            border: '1px solid var(--stroke)',
            borderRadius: 12,
            boxShadow: 'var(--shadow-panel)',
            minWidth: 180,
            padding: 5,
          }}
          onClick={(e) => e.stopPropagation()}
        >
          {rowActions.map((a, i) => (
            <button
              key={a.label}
              type="button"
              onClick={(e) => {
                e.stopPropagation()
                setMenuOpen(false)
                a.action()
              }}
              style={{
                display: 'flex',
                alignItems: 'center',
                gap: 9,
                width: '100%',
                padding: '8px 10px',
                borderRadius: 8,
                fontSize: 13,
                color: a.danger ? 'var(--danger)' : 'var(--text)',
                borderTop: i > 0 && rowActions[i - 1].label === '标记完成' && !a.danger ? '1px solid var(--divider)' : undefined,
              }}
              onMouseEnter={(e) => (e.currentTarget.style.background = 'var(--surface-raised)')}
              onMouseLeave={(e) => (e.currentTarget.style.background = 'transparent')}
            >
              <Icon name={a.icon} size={13} />
              {a.label}
            </button>
          ))}
        </div>
      ) : null}

      {dueOpen ? (
        <>
          <div
            style={{ position: 'fixed', inset: 0, zIndex: 40 }}
            onClick={() => setDueOpen(false)}
          />
          <DuePicker
            initial={item.dueDate ? new Date(item.dueDate) : null}
            onSave={(v) => {
              setDueOpen(false)
              void run(() => api.taskSetDue(item.id, v))
            }}
            onClose={() => setDueOpen(false)}
          />
        </>
      ) : null}

      {/* 行内占位保持结构完整 */}
      {null}
    </div>
  )
}
