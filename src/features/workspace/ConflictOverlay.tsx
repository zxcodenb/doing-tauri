// 冲突/首次仲裁选择面板（CloudChoiceView 语义）：必须先选择后确认。

import { useState } from 'react'
import { api } from '../../lib/ipc'
import { useDoing } from '../../hooks/useDoing'
import { ActionButton, Notice } from '../../components/ui'
import { Icon } from '../../components/icons'
import { formatDateInput } from '../../lib/time'

export function ConflictOverlay() {
  const { conflict, snapshot, run } = useDoing()
  const [choice, setChoice] = useState<'local' | 'cloud' | null>(null)
  if (!conflict) return null
  const localCount = snapshot.items.length

  const choose = async () => {
    if (!choice) return
    if (choice === 'local') {
      await run(() => api.conflictChooseLocal(conflict.cloudVersion))
    } else {
      await run(() => api.conflictChooseCloud(conflict.cloudVersion))
    }
    setChoice(null)
  }

  return (
    <div
      role="dialog"
      aria-modal
      style={{
        position: 'fixed',
        inset: 0,
        zIndex: 200,
        background: 'color-mix(in srgb, var(--bg) 72%, transparent)',
        backdropFilter: 'blur(2px)',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        padding: 20,
      }}
    >
      <div
        style={{
          width: 374,
          maxWidth: '100%',
          background: 'var(--bg)',
          borderRadius: 20,
          border: '1px solid var(--stroke)',
          boxShadow: 'var(--shadow-panel)',
          padding: 24,
          display: 'flex',
          flexDirection: 'column',
          gap: 15,
        }}
      >
        <div style={{ display: 'flex', alignItems: 'flex-start', gap: 13 }}>
          <span
            style={{
              width: 52,
              height: 52,
              borderRadius: 15,
              background: 'var(--violet-soft)',
              color: 'var(--violet)',
              display: 'inline-flex',
              alignItems: 'center',
              justifyContent: 'center',
              flex: 'none',
            }}
          >
            <Icon name="branch" size={26} />
          </span>
          <div>
            <div style={{ fontSize: 20, fontWeight: 700, color: 'var(--text)' }}>保留哪一份？</div>
            <div style={{ fontSize: 12, color: 'var(--text-dim)', lineHeight: 1.5, marginTop: 4 }}>
              此设备与云端都有事项。请选择一份继续使用，另一份会被替换。
            </div>
          </div>
        </div>

        <ChoiceCard
          icon="person"
          title="此设备的事项"
          count={localCount}
          detail="保留本地，并更新云端"
          selected={choice === 'local'}
          onSelect={() => setChoice('local')}
        />
        <ChoiceCard
          icon="icloud"
          title="云端的事项"
          count={conflict.cloudCount}
          detail="下载云端，并替换此设备"
          selected={choice === 'cloud'}
          onSelect={() => setChoice('cloud')}
        />
        {conflict.updatedAt ? (
          <div style={{ fontSize: 11, color: 'var(--text-faint)' }}>
            云端更新于 {formatDateInput(new Date(conflict.updatedAt))}
          </div>
        ) : null}

        {choice ? (
          <Notice
            isError
            symbol="exclamation"
            message={
              choice === 'local'
                ? '云端现有内容将被替换。'
                : '本地现有内容将被替换，此操作不能撤销。'
            }
          />
        ) : null}

        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <ActionButton
            kind="secondary"
            onClick={() => {
              setChoice(null)
              void run(() => api.conflictDefer())
            }}
          >
            稍后决定
          </ActionButton>
          <span style={{ flex: 1 }} />
          <ActionButton kind="primary" disabled={!choice} onClick={() => void choose()}>
            {choice === 'local' ? '保留此设备' : choice === 'cloud' ? '使用云端' : '选择后继续'}
          </ActionButton>
        </div>
        <div style={{ fontSize: 11, color: 'var(--text-dim)', lineHeight: 1.5 }}>
          稍后决定时，自动同步会暂停，不会覆盖任何一份。
        </div>
      </div>
    </div>
  )
}

function ChoiceCard({
  icon,
  title,
  count,
  detail,
  selected,
  onSelect,
}: {
  icon: 'person' | 'icloud'
  title: string
  count: number
  detail: string
  selected: boolean
  onSelect: () => void
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 12,
        padding: 14,
        borderRadius: 12,
        background: selected ? 'var(--violet-soft)' : 'var(--surface)',
        border: `1px solid ${selected ? 'var(--violet)' : 'var(--divider)'}`,
        textAlign: 'left',
      }}
    >
      <span style={{ color: 'var(--violet)', display: 'inline-flex', width: 26 }}>
        <Icon name={icon} size={20} />
      </span>
      <span style={{ flex: 1, minWidth: 0 }}>
        <span style={{ display: 'flex', alignItems: 'baseline', gap: 7 }}>
          <b style={{ fontSize: 14, color: 'var(--text)' }}>{title}</b>
          <span style={{ fontSize: 11, color: 'var(--text-dim)' }}>{count} 件</span>
        </span>
        <span style={{ display: 'block', fontSize: 12, color: 'var(--text-dim)', marginTop: 3 }}>
          {detail}
        </span>
      </span>
      <span style={{ color: selected ? 'var(--violet)' : 'var(--marker)', fontSize: 15 }}>
        {selected ? '◉' : '○'}
      </span>
    </button>
  )
}
