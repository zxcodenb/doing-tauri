import { useRef, useState, useEffect } from 'react'
import { ActionButton } from '../../components/ui'
import { useDoing } from '../../hooks/useDoing'
import { api } from '../../lib/ipc'
import type { MigrationStatus } from '../../types'

type Confirmation = 'import' | 'cancel' | 'keep' | null

export function MigrationPanel({ status }: { status: MigrationStatus }) {
  const { run } = useDoing()
  const [confirmation, setConfirmation] = useState<Confirmation>(null)
  const [preferences, setPreferences] = useState(status.preferencesAvailable)
  const [busy, setBusy] = useState(false)
  const [dismissed, setDismissed] = useState(false)
  const dialog = useRef<HTMLDivElement>(null)
  const title = confirmation === 'import' ? '确认导入旧版数据' : confirmation === 'cancel' ? '撤销本次导入？' : '保留当前新版文件？'

  useEffect(() => {
    if (!confirmation) return
    const previous = document.activeElement
    dialog.current?.querySelector<HTMLButtonElement>('button')?.focus()
    return () => { if (previous instanceof HTMLElement && previous.isConnected) previous.focus() }
  }, [confirmation])

  const execute = async (action: () => Promise<unknown>) => {
    if (busy) return
    setBusy(true)
    const result = await run(action)
    setBusy(false)
    if (result.ok) setConfirmation(null)
  }
  const confirm = () => {
    if (confirmation === 'import' && status.detectedFile) {
      void execute(() => api.migrationImport(status.detectedFile!, preferences && status.preferencesAvailable))
    } else if (confirmation === 'cancel' && status.transactionId) {
      void execute(() => api.migrationCancel(status.transactionId!))
    } else if (confirmation === 'keep' && status.transactionId) {
      void execute(() => api.migrationKeepCurrent(status.transactionId!))
    }
  }

  if ((!status.available && !status.recoveryRequired && !status.imported && !status.error) || (dismissed && !status.recoveryRequired)) return null
  return <>
    <section aria-label="旧版数据迁移" style={{ position: 'absolute', left: 12, right: 12, bottom: 12, maxHeight: '47%', overflowY: 'auto', background: 'var(--surface)', border: '1px solid var(--violet)', borderRadius: 12, padding: 12, boxShadow: 'var(--shadow-panel)', zIndex: 90, fontSize: 12, lineHeight: 1.5 }}>
      <strong>{status.recoveryRequired ? '旧版导入尚未完成' : status.imported ? '旧版导入记录' : '检测到旧版 Doing 数据'}</strong>
      <p style={{ margin: '6px 0' }}>{status.recoveryRequired
        ? '任务、偏好和认证恢复已暂停。可核对后继续恢复，或撤销本次尚未完成的发布；不会覆盖后续修改。'
        : status.imported
          ? status.requiresLogin ? '本地导入已完成，尚未上传。请重新登录，并确认这些数据的账号归属。' : '事项导入已完成；旧数据和迁移备份保留。'
          : '导入前会备份原文件；不会读取旧 Token。可以同时导入旧版偏好，导入后需要重新登录。'}</p>
      {status.error ? <p role="alert" style={{ color: 'var(--danger)', margin: '6px 0' }}>{status.error}</p> : null}
      {status.warnings.length ? <ul style={{ paddingLeft: 18, margin: '6px 0' }}>{status.warnings.map((warning) => <li key={warning}>{warning}</li>)}</ul> : null}
      {status.backupPath ? <p style={{ overflowWrap: 'anywhere', color: 'var(--text-dim)', fontSize: 10, margin: '6px 0' }}>备份：{status.backupPath}</p> : null}
      <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6 }}>
        {status.recoveryRequired ? <>
          <ActionButton kind="primary" disabled={busy || !status.transactionId} onClick={() => status.transactionId && void execute(() => api.migrationResume(status.transactionId!))}>{busy ? '处理中…' : '继续恢复'}</ActionButton>
          <ActionButton disabled={busy || !status.transactionId} onClick={() => setConfirmation('cancel')}>撤销本次导入</ActionButton>
          <ActionButton kind="quiet" disabled={busy || !status.transactionId} onClick={() => setConfirmation('keep')}>保留当前文件</ActionButton>
          <ActionButton kind="quiet" disabled={busy} onClick={() => void run(api.systemRevealData)}>打开数据目录</ActionButton>
        </> : <>
          {status.available ? <ActionButton kind="primary" disabled={busy || !status.detectedFile} onClick={() => setConfirmation('import')}>导入</ActionButton> : null}
          <ActionButton kind="quiet" disabled={busy} onClick={() => setDismissed(true)}>{status.imported ? '知道了' : '稍后决定'}</ActionButton>
        </>}
      </div>
    </section>
    {confirmation ? <div style={{ position: 'absolute', inset: 0, background: 'var(--backdrop, rgba(0,0,0,.35))', display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 130, padding: 16 }}>
      <div ref={dialog} role="dialog" aria-modal="true" aria-label={title}
        onKeyDown={(event) => {
          if (event.nativeEvent.isComposing) return
          if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); if (!busy) setConfirmation(null) }
          if (event.key === 'Tab') {
            const elements = dialog.current?.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled)')
            if (!elements?.length) return
            const first = elements[0], last = elements[elements.length - 1]
            if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus() }
            if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus() }
          }
        }}
        style={{ width: '100%', maxWidth: 360, maxHeight: '90%', overflowY: 'auto', background: 'var(--surface)', color: 'var(--text)', border: '1px solid var(--stroke)', borderRadius: 14, padding: 18, fontSize: 12, lineHeight: 1.6 }}>
        <h2 style={{ fontSize: 16, marginTop: 0 }}>{title}</h2>
        <p>{confirmation === 'import'
          ? '请先手动退出旧版 Doing。仅在新版尚未初始化时导入；原始文件保留。导入后重新登录并确认归属，才允许上传。'
          : confirmation === 'cancel'
            ? '仅撤销本次仍未被后续修改的发布，恢复原新版偏好。旧文件和迁移备份不会删除；发现后续修改会停止，不会强行回滚。'
            : '不再恢复或回滚此事务，保留现在磁盘上的新版文件（任务和偏好可能尚未全部导入）。这不会标记迁移成功。备份保留，之后需要重新登录。'}</p>
        {confirmation === 'import' && status.preferencesAvailable ? <label style={{ display: 'flex', gap: 8, alignItems: 'flex-start' }}>
          <input type="checkbox" checked={preferences} disabled={busy} onChange={(event) => setPreferences(event.target.checked)} />
          同时导入旧版外观和提醒偏好（替换本机新版偏好；不继承通知授权和登录项）
        </label> : null}
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8, marginTop: 16 }}>
          <ActionButton disabled={busy} onClick={() => setConfirmation(null)}>返回</ActionButton>
          <ActionButton kind={confirmation === 'import' ? 'primary' : 'destructive'} disabled={busy} onClick={confirm}>
            {busy ? '处理中…' : confirmation === 'import' ? '确认导入' : confirmation === 'cancel' ? '确认撤销' : '确认保留当前文件'}
          </ActionButton>
        </div>
      </div>
    </div> : null}
  </>
}
