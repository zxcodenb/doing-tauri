// 同步状态徽标与详情弹层（SyncBadge / SyncStatusView 语义）。

import { useState } from 'react'
import { api } from '../../lib/ipc'
import { useDoing } from '../../hooks/useDoing'
import { SYNC_TEXT, type SyncStateName } from '../../types'
import { ActionButton, Notice } from '../../components/ui'
import { Icon, type IconName } from '../../components/icons'

function iconOf(state: SyncStateName): { name: IconName; color: string } {
  switch (state) {
    case 'idle':
      return { name: 'icloud', color: 'var(--text-dim)' }
    case 'pending':
      return { name: 'clockCircle', color: 'var(--violet)' }
    case 'syncing':
      return { name: 'arrowRetry', color: 'var(--violet)' }
    case 'synced':
      return { name: 'checkIcloud', color: 'var(--text-dim)' }
    case 'failed':
      return { name: 'exclamation', color: 'var(--danger)' }
    case 'conflict':
      return { name: 'branch', color: 'var(--danger)' }
    case 'unauthorized':
      return { name: 'personBadge', color: 'var(--danger)' }
  }
}

export function SyncBadge() {
  const { sync, settings, conflict, showConflict, run } = useDoing()
  const [open, setOpen] = useState(false)
  if (!sync) return null
  const autoOff = settings && !settings.automaticSync && (sync.state === 'idle' || sync.state === 'synced')
  const label = autoOff ? '手动同步' : sync.state === 'idle' ? '云端同步' : SYNC_TEXT[sync.state]
  const meta = iconOf(sync.state)

  return (
    <span style={{ position: 'relative', display: 'inline-flex' }}>
      <button
        type="button"
        title="查看同步详情"
        onClick={() => setOpen((v) => !v)}
        style={{
          display: 'inline-flex',
          alignItems: 'center',
          gap: 5,
          fontSize: 11,
          color: meta.color,
          padding: '3px 0',
        }}
      >
        <Icon name={meta.name} size={12} />
        {label}
      </button>
      {open ? (
        <>
          <div style={{ position: 'fixed', inset: 0, zIndex: 70 }} onClick={() => setOpen(false)} />
          <div
            style={{
              position: 'absolute',
              bottom: 22,
              right: 0,
              zIndex: 71,
              width: 300,
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
              <Icon name={meta.name} size={15} />
              <b style={{ fontSize: 13, color: meta.color }}>{SYNC_TEXT[sync.state]}</b>
            </div>
            {sync.lastError ? (
              <Notice message={sync.lastError} isError />
            ) : (
              <div style={{ fontSize: 12, color: 'var(--text-dim)', lineHeight: 1.5 }}>
                {settings?.automaticSync
                  ? '本地修改会自动同步到你的账户。'
                  : '自动同步已关闭，任务仍保存在此设备。'}
              </div>
            )}
            {sync.lastSyncAt ? (
              <div style={{ fontSize: 11, color: 'var(--text-dim)' }}>
                最近成功：{new Date(sync.lastSyncAt).toLocaleString('zh-CN', { hour12: false })}
              </div>
            ) : null}
            <div style={{ display: 'flex', gap: 8 }}>
              <ActionButton
                kind="primary"
                disabled={sync.state === 'syncing'}
                onClick={() => {
                  if (sync.state === 'conflict' && conflict) { showConflict(); setOpen(false) }
                  else void run(() => api.syncFlush())
                }}
              >
                {sync.state === 'conflict' ? (conflict ? '处理冲突' : '重试获取候选') : sync.state === 'failed' ? '重试同步' : '立即同步'}
              </ActionButton>
              <ActionButton onClick={() => void api.systemOpenSettings()}>同步设置</ActionButton>
            </div>
          </div>
        </>
      ) : null}
    </span>
  )
}
