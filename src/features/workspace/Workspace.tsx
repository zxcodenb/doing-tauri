// 主工作区：标题、焦点卡、待办列表、已完成折叠、录入行、反馈条。
// 键盘导航：↑↓ 选择、空格完成、回车编辑、退格删除、Esc 取消选择。

import { useEffect, useMemo, useRef, useState } from 'react'
import { api } from '../../lib/ipc'
import { useDoing } from '../../hooks/useDoing'
import { activeCount, countOverdue, isOverdueNow, orderItems } from '../../lib/ordering'
import { dueStampText } from '../../lib/time'
import type { ItemView } from '../../types'
import { Icon } from '../../components/icons'
import { CountBadge } from '../../components/ui'
import { DuePicker } from '../../components/DuePicker'
import { ChromeBar } from './ChromeBar'
import { TaskRow } from './TaskRow'
import { SyncBadge } from './SyncBadge'

export function Workspace({ panelMode }: { panelMode: boolean }) {
  const { snapshot, settings, auth, notices, dismissNotice, run, now } = useDoing()
  const [editingId, setEditingId] = useState<string | null>(null)
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [showsCompleted, setShowsCompleted] = useState(false)
  const [draft, setDraft] = useState('')
  const [draftDue, setDraftDue] = useState<string | null>(null)
  const [dueOpen, setDueOpen] = useState(false)
  const rowRefs = useRef(new Map<string, HTMLDivElement>())
  const inputRef = useRef<HTMLInputElement>(null)

  const { focus, remaining, completed } = useMemo(
    () => orderItems(snapshot.items, snapshot.focusId),
    [snapshot.items, snapshot.focusId],
  )
  const totalActive = activeCount(snapshot.items)
  const overdue = isOverdueNow(snapshot.items, now)
  const overdueCount = countOverdue(snapshot.items, now)
  const showOverdueBanner = settings?.showOverdueBanner ?? true

  // 通知/点击定位滚动。
  useEffect(() => {
    const handler = (e: Event) => {
      const id = (e as CustomEvent<string>).detail
      if (!id) return
      const item = snapshot.items.find((i) => i.id === id)
      if (item?.done) setShowsCompleted(true)
      setSelectedId(id)
      rowRefs.current.get(id)?.scrollIntoView({ block: 'center', behavior: 'smooth' })
    }
    window.addEventListener('doing://scroll-to-item', handler)
    return () => window.removeEventListener('doing://scroll-to-item', handler)
  }, [snapshot.items])

  // 远端替换后清理失效状态；选择越界时收敛。
  useEffect(() => {
    if (editingId && !snapshot.items.some((i) => i.id === editingId)) setEditingId(null)
  }, [snapshot.items, editingId])

  const navItems = showsCompleted ? [...remaining, ...completed] : remaining

  useEffect(() => {
    if (selectedId && !navItems.some((i) => i.id === selectedId)) setSelectedId(null)
  }, [navItems, selectedId])

  // ⌘N / 窗口出现聚焦到录入框；内容就绪（数据到位）后首次聚焦。
  const contentReady = Boolean(settings && auth?.loggedIn)
  useEffect(() => {
    if (!contentReady) return
    const focusInput = () => {
      if (!editingId) inputRef.current?.focus()
    }
    window.addEventListener('doing://focus-request', focusInput)
    window.addEventListener('doing://window-focused', focusInput)
    focusInput()
    return () => {
      window.removeEventListener('doing://focus-request', focusInput)
      window.removeEventListener('doing://window-focused', focusInput)
    }
  }, [contentReady, editingId])

  const focusRow = (id: string) => {
    rowRefs.current.get(id)?.focus({ preventScroll: true })
  }

  const moveSelection = (offset: number) => {
    if (!navItems.length) {
      setSelectedId(null)
      return
    }
    const idx = navItems.findIndex((i) => i.id === selectedId)
    const next =
      idx === -1 ? (offset > 0 ? 0 : navItems.length - 1) : Math.min(Math.max(idx + offset, 0), navItems.length - 1)
    const item = navItems[next]
    setSelectedId(item.id)
    focusRow(item.id)
    rowRefs.current.get(item.id)?.scrollIntoView({ block: 'center' })
  }

  const submitDraft = async () => {
    if (!draft.trim()) return
    const { ok, message } = await run(() => api.taskAdd(draft.trim(), draftDue))
    if (ok) {
      setDraft('')
      setDraftDue(null)
      inputRef.current?.focus()
    }
    void message
  }

  const handleListKeyDown = (e: React.KeyboardEvent) => {
    const target = e.target as HTMLElement
    // 录入框内：↑↓ 仍接管（导航选择并移交行焦点），空格/回车/退格保留原生输入语义；
    // 行内编辑器（textarea）完全不接管（其自身处理提交与取消）。
    const inComposer = target === inputRef.current
    const inEditor = !inComposer && (target.tagName === 'TEXTAREA' || target.tagName === 'INPUT')
    if (e.nativeEvent.isComposing || inEditor || e.metaKey || e.ctrlKey || e.altKey) return
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault()
        moveSelection(1)
        break
      case 'ArrowUp':
        e.preventDefault()
        moveSelection(-1)
        break
      case ' ':
        if (selectedId && !inComposer) {
          e.preventDefault()
          void run(() => api.taskToggleDone(selectedId))
        }
        break
      case 'Enter':
        if (selectedId && !inComposer) {
          e.preventDefault()
          setEditingId(selectedId)
        }
        break
      case 'Backspace':
      case 'Delete':
        if (selectedId && !inComposer) {
          e.preventDefault()
          void run(() => api.taskDelete(selectedId))
        }
        break
      case 'Escape':
        if (inComposer) break // 录入框自行处理：先清草稿，空草稿再收起窗口
        if (selectedId) setSelectedId(null)
        else void api.systemHideMain()
        break
    }
  }

  if (!settings || !auth?.loggedIn) return null

  const row = (item: ItemView, isFocus = false) => (
    <TaskRow
      item={item}
      isFocus={isFocus}
      selected={selectedId === item.id}
      editing={editingId === item.id}
      beginEdit={() => {
        setSelectedId(item.id)
        setEditingId(item.id)
      }}
      endEdit={() => setEditingId(null)}
      now={now}
      settings={settings}
      onSelect={() => {
        setSelectedId(item.id)
        focusRow(item.id)
      }}
    />
  )

  return (
    <div
      style={{ height: '100%', display: 'flex', flexDirection: 'column', background: 'var(--bg)' }}
      onKeyDown={handleListKeyDown}
    >
      <ChromeBar panelMode={panelMode} />
      <div style={{ display: 'flex', alignItems: 'baseline', padding: '6px 20px 18px' }}>
        <span style={{ fontSize: 27, fontWeight: 800, letterSpacing: -0.8, color: 'var(--text)' }}>
          现在，就做。
        </span>
        <span style={{ flex: 1 }} />
        <span style={{ fontSize: 12, color: 'var(--text-dim)' }}>{totalActive} 件待办</span>
      </div>

      <div style={{ flex: 1, minHeight: 0, display: 'flex', flexDirection: 'column' }}>
        <div style={{ flex: 1, overflowY: 'auto', padding: '0 20px 8px' }}>
          {focus ? (
            <div
              style={{
                background: 'var(--focus)',
                borderRadius: 15,
                padding: '10px 12px 12px',
                marginBottom: 16,
              }}
            >
              <div
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: 6,
                  fontSize: 11,
                  fontWeight: 600,
                  letterSpacing: 1.4,
                  color: 'var(--focus-dim)',
                  padding: '0 8px 4px',
                }}
              >
                <Icon name="scope" size={12} />
                FOCUS · 当前焦点
              </div>
              {row(focus, true)}
            </div>
          ) : null}

          {totalActive === 0 ? (
            <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 12, padding: '38px 0' }}>
              <span
                style={{
                  width: 68,
                  height: 68,
                  borderRadius: 21,
                  background: 'var(--violet-soft)',
                  color: 'var(--violet)',
                  display: 'inline-flex',
                  alignItems: 'center',
                  justifyContent: 'center',
                }}
              >
                <Icon name={completed.length > 0 ? 'check' : 'sparkle'} size={30} strokeWidth={1.6} />
              </span>
              <span style={{ fontSize: 14, fontWeight: 600, color: 'var(--text)' }}>
                {completed.length > 0 ? '都做完了。' : '给想做的事，一个开始。'}
              </span>
              <span style={{ fontSize: 12, color: 'var(--text-dim)' }}>
                {completed.length > 0 ? '留点空白，也很好。' : '在下方记一件事，回车就好。'}
              </span>
            </div>
          ) : null}

          {remaining.length > 0 ? (
            <section style={{ marginBottom: 6 }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 7, padding: '0 5px 8px' }}>
                <span style={{ fontSize: 12, fontWeight: 600, color: 'var(--text)' }}>
                  {focus ? '接下来' : '待办事项'}
                </span>
                <CountBadge count={remaining.length} />
                <span style={{ flex: 1 }} />
                {showOverdueBanner && overdue ? (
                  <span style={{ display: 'inline-flex', alignItems: 'center', gap: 4, fontSize: 11, color: 'var(--danger)' }}>
                    <Icon name="exclamation" size={12} />
                    {overdueCount} 件逾期
                  </span>
                ) : null}
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 2 }}>
                {remaining.map((item) => (
                  <div
                    key={item.id}
                    tabIndex={-1}
                    style={{ outline: 'none' }}
                    ref={(el) => {
                      if (el) rowRefs.current.set(item.id, el)
                      else rowRefs.current.delete(item.id)
                    }}
                  >
                    {row(item)}
                  </div>
                ))}
              </div>
            </section>
          ) : null}

          {completed.length > 0 ? (
            <section style={{ marginTop: 4 }}>
              <div style={{ display: 'flex', alignItems: 'center', padding: '2px 6px' }}>
                <button
                  type="button"
                  onClick={() => setShowsCompleted((v) => !v)}
                  style={{
                    display: 'inline-flex',
                    alignItems: 'center',
                    gap: 6,
                    fontSize: 12,
                    color: 'var(--text-dim)',
                    padding: '7px 0',
                  }}
                >
                  <Icon name={showsCompleted ? 'chevronDown' : 'chevronRight'} size={11} />
                  已完成 · {completed.length}
                </button>
                <span style={{ flex: 1 }} />
                {showsCompleted ? (
                  <button
                    type="button"
                    title="清除已完成事项，可以撤销"
                    style={{ fontSize: 12, color: 'var(--text-dim)' }}
                    onClick={() => void run(() => api.taskClearCompleted())}
                  >
                    清除
                  </button>
                ) : null}
              </div>
              {showsCompleted ? (
                <div style={{ display: 'flex', flexDirection: 'column', gap: 2 }}>
                  {completed.map((item) => (
                    <div
                      key={item.id}
                      tabIndex={-1}
                      style={{ outline: 'none' }}
                      ref={(el) => {
                        if (el) rowRefs.current.set(item.id, el)
                        else rowRefs.current.delete(item.id)
                      }}
                    >
                      {row(item)}
                    </div>
                  ))}
                </div>
              ) : null}
            </section>
          ) : null}
        </div>

        <div
          style={{
            padding: '8px 20px 14px',
            display: 'flex',
            flexDirection: 'column',
            gap: 8,
            background: 'var(--bg)',
          }}
        >
          {notices.length > 0 ? (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
              {notices.slice(-2).map((n) => (
                <div
                  key={n.id}
                  role={n.kind === 'error' ? 'alert' : 'status'}
                  style={{
                    display: 'flex',
                    alignItems: 'center',
                    gap: 8,
                    padding: '7px 10px',
                    borderRadius: 10,
                    background: n.kind === 'error' ? 'var(--danger-soft)' : 'var(--surface-raised)',
                    fontSize: 12,
                    color: n.kind === 'error' ? 'var(--danger)' : 'var(--text)',
                  }}
                >
                  <Icon name={n.kind === 'error' ? 'exclamation' : 'check'} size={12} />
                  <span style={{ flex: 1, lineHeight: 1.3 }}>{n.message}</span>
                  {n.offersUndo ? (
                    <button
                      type="button"
                      style={{ fontSize: 12, fontWeight: 600, color: 'var(--violet)' }}
                      onClick={() => {
                        dismissNotice(n.id)
                        void run(() => api.historyUndo())
                      }}
                    >
                      撤销
                    </button>
                  ) : null}
                  <button type="button" aria-label="关闭提示" onClick={() => dismissNotice(n.id)} style={{ color: 'var(--text-dim)' }}>
                    <Icon name="x" size={11} />
                  </button>
                </div>
              ))}
            </div>
          ) : null}

          {/* 录入行 */}
          <div style={{ position: 'relative' }}>
            <div
              style={{
                display: 'flex',
                alignItems: 'center',
                gap: 9,
                padding: 7,
                background: 'var(--surface)',
                borderRadius: 12,
                border: '1px solid var(--stroke)',
              }}
            >
              <button
                type="button"
                aria-label="添加事项（回车）"
                title="添加事项（回车）"
                onClick={() => void submitDraft()}
                style={{
                  width: 32,
                  height: 32,
                  borderRadius: 9,
                  background: 'var(--accent)',
                  color: 'var(--on-accent)',
                  display: 'inline-flex',
                  alignItems: 'center',
                  justifyContent: 'center',
                  flex: 'none',
                }}
              >
                <Icon name="plus" size={15} strokeWidth={2.4} />
              </button>
              <input
                ref={inputRef}
                value={draft}
                placeholder="记一件事…"
                aria-label="新事项"
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={async (e) => {
                  if (e.key === 'Enter' && !e.nativeEvent.isComposing && !e.metaKey) {
                    e.preventDefault()
                    await submitDraft()
                  } else if (e.key === 'Escape') {
                    e.preventDefault()
                    if (draft.trim() || draftDue) {
                      setDraft('')
                      setDraftDue(null)
                    } else {
                      await api.systemHideMain()
                    }
                  } else if (e.key === 'n' && e.metaKey) {
                    e.preventDefault()
                    inputRef.current?.focus()
                  }
                }}
                style={{
                  flex: 1,
                  minWidth: 0,
                  border: 'none',
                  outline: 'none',
                  background: 'transparent',
                  color: 'var(--text)',
                  fontSize: 14,
                  fontFamily: 'inherit',
                }}
              />
              <button
                type="button"
                aria-label="设置新事项的截止时间"
                title="设置新事项的截止时间"
                onClick={() => setDueOpen((v) => !v)}
                style={{
                  width: 28,
                  height: 28,
                  borderRadius: 7,
                  display: 'inline-flex',
                  alignItems: 'center',
                  justifyContent: 'center',
                  color: draftDue ? 'var(--violet)' : 'var(--text-dim)',
                  background: draftDue ? 'var(--violet-soft)' : 'transparent',
                  flex: 'none',
                }}
              >
                <Icon name="calendar" size={13} />
              </button>
            </div>
            {dueOpen ? (
              <>
                <div style={{ position: 'fixed', inset: 0, zIndex: 40 }} onClick={() => setDueOpen(false)} />
                <DuePicker
                  initial={draftDue ? new Date(draftDue) : null}
                  onSave={(v) => {
                    setDueOpen(false)
                    setDraftDue(v)
                  }}
                  onClose={() => setDueOpen(false)}
                />
              </>
            ) : null}
          </div>

          {draftDue ? (
            <div style={{ display: 'flex', alignItems: 'center', gap: 6, padding: '0 5px' }}>
              <span style={{ fontSize: 12, fontWeight: 600, color: 'var(--violet)' }}>
                {dueStampText(draftDue, now)}
              </span>
              <button
                type="button"
                aria-label="移除新事项的截止时间"
                onClick={() => setDraftDue(null)}
                style={{ color: 'var(--text-dim)', display: 'inline-flex' }}
              >
                <Icon name="x" size={12} />
              </button>
              <span style={{ flex: 1 }} />
              <span style={{ fontSize: 11, color: 'var(--text-dim)' }}>↵ 添加</span>
            </div>
          ) : null}

          <div style={{ display: 'flex', alignItems: 'center', fontSize: 11, color: 'var(--text-dim)', padding: '0 4px' }}>
            <span>⌘N 快速记录</span>
            <span style={{ flex: 1 }} />
            <SyncBadge />
          </div>
        </div>
      </div>
    </div>
  )
}
