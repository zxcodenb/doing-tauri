// 截止时间弹层（对应 DueEditor/DueStamp/DuePreset）。

import { useMemo, useState } from 'react'
import { ActionButton, IconButton } from '../components/ui'
import {
  combineLocal,
  DUE_PRESETS,
  dueStampText,
  formatDateInput,
  formatTimeInput,
  nextHour,
  presetDate,
  type DuePresetKind,
} from '../lib/time'
import { classifyDue, type DueStateKind } from '../theme/tokens'

export interface DueEditorHandle {
  open: (value: Date | null) => void
}

export function dueStateColor(kind: DueStateKind, isFocus = false): string {
  if (isFocus) {
    return kind === 'overdue' ? 'var(--danger)' : 'var(--focus-dim)'
  }
  switch (kind) {
    case 'overdue':
      return 'var(--danger)'
    case 'dueSoon':
      return 'var(--warn)'
    default:
      return 'var(--text-dim)'
  }
}

export function DueChip({
  due,
  now,
  dueSoonEnabled,
  dueSoonHours,
  isFocus = false,
  onClick,
}: {
  due: string
  now: Date
  dueSoonEnabled: boolean
  dueSoonHours: number
  isFocus?: boolean
  onClick: (e: React.MouseEvent) => void
}) {
  const kind = classifyDue(due, false, now, dueSoonEnabled, dueSoonHours)
  return (
    <button
      type="button"
      title="修改截止时间"
      onClick={onClick}
      style={{
        display: 'inline-flex',
        alignItems: 'center',
        gap: 4,
        fontSize: 11,
        fontVariantNumeric: 'tabular-nums',
        color: dueStateColor(kind, isFocus),
        padding: '2px 0',
      }}
    >
      <span>{kind === 'overdue' ? '已逾期 · ' : ''}{dueStampText(due, now)}</span>
    </button>
  )
}

/** 弹层容器：绝对定位在锚点下方（由调用方放在相对定位父级）。 */
export function DuePicker({
  initial,
  onSave,
  onClose,
}: {
  initial: Date | null
  /** 保存值：RFC3339 字符串或 null（移除）。 */
  onSave: (value: string | null) => void
  onClose: () => void
}) {
  const [dateText, setDateText] = useState(() => formatDateInput(initial ?? nextHour(new Date())))
  const [timeText, setTimeText] = useState(() => formatTimeInput(initial ?? nextHour(new Date())))
  const [selectedPreset, setSelectedPreset] = useState<DuePresetKind | null>(null)
  // 打开弹层时固定“现在”，避免渲染期调用不纯函数（React purity 规则）。
  const [nowTs] = useState(() => Date.now())
  const selected = useMemo(() => combineLocal(dateText, timeText), [dateText, timeText])
  const past = selected.getTime() <= nowTs

  const pickPreset = (kind: DuePresetKind) => {
    const d = presetDate(kind, new Date())
    setDateText(formatDateInput(d))
    setTimeText(formatTimeInput(d))
    setSelectedPreset(kind)
  }

  return (
    <div
      role="dialog"
      aria-label="截止时间"
      style={{
        position: 'absolute',
        zIndex: 60,
        top: 'calc(100% + 6px)',
        right: 0,
        width: 320,
        background: 'var(--bg)',
        border: '1px solid var(--stroke)',
        borderRadius: 14,
        boxShadow: 'var(--shadow-panel)',
        padding: 16,
        display: 'flex',
        flexDirection: 'column',
        gap: 12,
      }}
    >
      <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        <span style={{ color: 'var(--violet)' }}>
          <b>截止时间</b>
        </span>
        <span style={{ flex: 1 }} />
        <IconButton icon="x" help="关闭，不保存" onAction={onClose} />
      </div>
      <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 8 }}>
        {DUE_PRESETS.map((p) => {
          const active = selectedPreset === p.kind
          return (
            <button
              key={p.kind}
              type="button"
              onClick={() => pickPreset(p.kind)}
              style={{
                fontSize: 12,
                fontWeight: 600,
                color: active ? 'var(--violet)' : 'var(--text-dim)',
                background: active ? 'var(--violet-soft)' : 'var(--surface)',
                borderRadius: 8,
                padding: '10px 12px',
                textAlign: 'left',
                display: 'flex',
                justifyContent: 'space-between',
                alignItems: 'center',
              }}
            >
              {p.title}
              {active ? <span>✓</span> : null}
            </button>
          )
        })}
      </div>
      <div
        style={{
          background: 'var(--surface)',
          borderRadius: 10,
          padding: 12,
          display: 'flex',
          flexDirection: 'column',
          gap: 9,
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <span style={{ fontSize: 12, color: 'var(--text-dim)', width: 34 }}>日期</span>
          <input
            type="date"
            value={dateText}
            onChange={(e) => {
              setDateText(e.target.value)
              setSelectedPreset(null)
            }}
            style={{
              flex: 1,
              border: '1px solid var(--stroke)',
              borderRadius: 8,
              padding: '5px 8px',
              background: 'var(--surface)',
              color: 'var(--text)',
              font: 'inherit',
              fontSize: 13,
            }}
          />
        </div>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <span style={{ fontSize: 12, color: 'var(--text-dim)', width: 34 }}>时间</span>
          <input
            type="time"
            value={timeText}
            onChange={(e) => {
              setTimeText(e.target.value)
              setSelectedPreset(null)
            }}
            style={{
              flex: 1,
              border: '1px solid var(--stroke)',
              borderRadius: 8,
              padding: '5px 8px',
              background: 'var(--surface)',
              color: 'var(--text)',
              font: 'inherit',
              fontSize: 13,
            }}
          />
        </div>
        {past ? (
          <div style={{ fontSize: 12, color: 'var(--danger)' }}>
            这个时间已过去，保存后会标记为逾期。
          </div>
        ) : null}
      </div>
      <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
        {initial ? (
          <ActionButton kind="quiet" onClick={() => onSave(null)}>
            移除
          </ActionButton>
        ) : null}
        <span style={{ flex: 1 }} />
        <ActionButton kind="secondary" onClick={onClose}>
          取消
        </ActionButton>
        <ActionButton kind="primary" onClick={() => onSave(selected.toISOString())}>
          保存时间
        </ActionButton>
      </div>
    </div>
  )
}
