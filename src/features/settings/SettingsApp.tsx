// 设置窗口：通用 / 提醒 / 账户与同步 / 数据与关于。

import { useEffect, useState } from 'react'
import { getVersion } from '@tauri-apps/api/app'
import { api } from '../../lib/ipc'
import { useDoing } from '../../hooks/useDoing'
import { ActionButton, DoingMark, Notice, SurfaceCard, Wordmark } from '../../components/ui'
import { Icon, type IconName } from '../../components/icons'
import { SYNC_TEXT, type SettingsView } from '../../types'

type SectionId = 'general' | 'reminders' | 'account' | 'data'

const SECTIONS: { id: SectionId; title: string; subtitle: string; icon: IconName }[] = [
  { id: 'general', title: '通用', subtitle: '让 Doing 适应你的工作方式。', icon: 'gear' },
  { id: 'reminders', title: '提醒', subtitle: '该提醒的时候出现，其余时间保持安静。', icon: 'bell' },
  { id: 'account', title: '账户与同步', subtitle: '在此设备和云端之间，保持一致。', icon: 'person' },
  { id: 'data', title: '数据与关于', subtitle: '你的事项，始终保存在你的设备上。', icon: 'drive' },
]

function Row({
  title,
  detail,
  control,
  children,
}: {
  title: string
  detail?: string
  control?: React.ReactNode
  children?: React.ReactNode
}) {
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 15, minHeight: 32 }}>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ fontSize: 14, color: 'var(--text)' }}>{title}</div>
        {detail ? (
          <div style={{ fontSize: 12, color: 'var(--text-dim)', marginTop: 2, lineHeight: 1.4 }}>
            {detail}
          </div>
        ) : null}
      </div>
      {control ?? children}
    </div>
  )
}

function Group({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 9 }}>
      <div style={{ fontSize: 12, fontWeight: 600, color: 'var(--text-dim)', paddingLeft: 2 }}>{title}</div>
      <SurfaceCard>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 15 }}>{children}</div>
      </SurfaceCard>
    </div>
  )
}

function Toggle({
  checked,
  onChange,
  disabled = false,
}: {
  checked: boolean
  onChange: (v: boolean) => void
  disabled?: boolean
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      style={{
        width: 40,
        height: 24,
        borderRadius: 12,
        background: checked ? 'var(--violet)' : 'var(--surface-pressed)',
        position: 'relative',
        transition: 'background 0.15s',
        opacity: disabled ? 0.5 : 1,
        flex: 'none',
      }}
    >
      <span
        style={{
          position: 'absolute',
          top: 2.5,
          left: checked ? 18.5 : 2.5,
          width: 19,
          height: 19,
          borderRadius: '50%',
          background: '#fff',
          transition: 'left 0.15s',
        }}
      />
    </button>
  )
}

function Segmented<T extends string>({
  value,
  options,
  onChange,
  width = 220,
}: {
  value: T
  options: { value: T; label: string }[]
  onChange: (v: T) => void
  width?: number
}) {
  return (
    <div
      style={{
        display: 'inline-flex',
        background: 'var(--surface-raised)',
        borderRadius: 9,
        padding: 2,
        width,
      }}
    >
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          onClick={() => onChange(o.value)}
          style={{
            flex: 1,
            padding: '6px 4px',
            borderRadius: 7,
            fontSize: 12,
            fontWeight: 600,
            color: value === o.value ? 'var(--violet)' : 'var(--text-dim)',
            background: value === o.value ? 'var(--bg)' : 'transparent',
          }}
        >
          {o.label}
        </button>
      ))}
    </div>
  )
}

const DUE_SOON_OPTIONS = [
  { value: 0.25, label: '15 分钟' },
  { value: 1, label: '1 小时' },
  { value: 6, label: '6 小时' },
  { value: 24, label: '24 小时' },
  { value: 48, label: '48 小时' },
]

export function SettingsApp() {
  const { settings, sync, auth, snapshot, run } = useDoing()
  const [section, setSection] = useState<SectionId>('general')
  const [launchAtLogin, setLaunchAtLogin] = useState(false)
  const [launchMessage, setLaunchMessage] = useState<string | null>(null)
  const [notice, setNotice] = useState<{ kind: 'ok' | 'err'; text: string } | null>(null)
  const [confirmReset, setConfirmReset] = useState(false)
  const [confirmLogout, setConfirmLogout] = useState(false)
  const [version, setVersion] = useState('')
  const [notifGranted, setNotifGranted] = useState<boolean | null>(null)

  useEffect(() => {
    void getVersion().then(setVersion)
    void api.launchAtLoginGet().then(setLaunchAtLogin)
    void import('@tauri-apps/plugin-notification')
      .then((m) => m.isPermissionGranted())
      .then(setNotifGranted)
      .catch(() => {})
  }, [])

  // 托盘入口（如“从云端恢复…”）要求定位到账户板块：窗口获得焦点时消费一次性请求。
  useEffect(() => {
    const applyRequested = () => {
      try {
        const s = localStorage.getItem('doing.openSettingsSection')
        if (s === 'general' || s === 'reminders' || s === 'account' || s === 'data') {
          setSection(s)
          localStorage.removeItem('doing.openSettingsSection')
        }
      } catch {
        /* 忽略存储异常 */
      }
    }
    applyRequested()
    window.addEventListener('focus', applyRequested)
    return () => window.removeEventListener('focus', applyRequested)
  }, [])

  if (!settings) return null

  const patch = (p: Partial<SettingsView>) => {
    void run(() => api.settingsUpdate(p))
  }

  const meta = SECTIONS.find((s) => s.id === section)!

  const activeCount = snapshot.items.filter((i) => !i.done).length
  const doneCount = snapshot.items.filter((i) => i.done).length

  const setLogin = async (v: boolean) => {
    const message = await api.launchAtLoginSet(v)
    setLaunchAtLogin(v)
    setLaunchMessage(message || null)
  }

  return (
    <div style={{ display: 'flex', height: '100%', background: 'var(--bg)' }}>
      {/* 侧栏 */}
      <div
        style={{
          width: 176,
          flex: 'none',
          background: 'var(--sidebar)',
          display: 'flex',
          flexDirection: 'column',
          paddingTop: 8,
        }}
      >
        <div style={{ padding: '14px 19px 20px' }}>
          <Wordmark />
        </div>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 5, padding: '0 10px' }}>
          {SECTIONS.map((s) => (
            <button
              key={s.id}
              type="button"
              onClick={() => setSection(s.id)}
              style={{
                display: 'flex',
                alignItems: 'center',
                gap: 10,
                padding: '0 12px',
                height: 39,
                borderRadius: 9,
                fontSize: 12,
                fontWeight: 600,
                color: section === s.id ? 'var(--violet)' : 'var(--text-dim)',
                background: section === s.id ? 'var(--violet-soft)' : 'transparent',
              }}
            >
              <Icon name={s.icon} size={14} />
              {s.title}
            </button>
          ))}
        </div>
        <div style={{ flex: 1 }} />
        <div style={{ fontSize: 12, lineHeight: 1.6, color: 'var(--text-dim)', padding: '0 21px 22px' }}>
          少一点惦记。
          <br />
          多一点完成。
        </div>
      </div>

      {/* 内容 */}
      <div style={{ flex: 1, minWidth: 0, display: 'flex', flexDirection: 'column' }}>
        <div style={{ padding: '20px 26px 16px' }}>
          <div style={{ fontSize: 21, fontWeight: 700, color: 'var(--text)' }}>{meta.title}</div>
          <div style={{ fontSize: 12, color: 'var(--text-dim)', marginTop: 4 }}>{meta.subtitle}</div>
        </div>
        <div style={{ flex: 1, overflowY: 'auto', padding: '0 26px 26px' }}>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 22 }}>
            {section === 'general' ? (
              <>
                <Group title="外观">
                  <Row title="配色" detail="为白天和夜晚分别设计。">
                    <Segmented
                      value={settings.appearance}
                      width={218}
                      options={[
                        { value: 'system', label: '跟随系统' },
                        { value: 'light', label: '浅色' },
                        { value: 'dark', label: '深色' },
                      ]}
                      onChange={(v) => patch({ appearance: v })}
                    />
                  </Row>
                </Group>
                <Group title="启动与窗口">
                  <Row title="打开方式" detail="随手展开，或留在桌面。">
                    <Segmented
                      value={settings.mode}
                      width={184}
                      options={[
                        { value: 'popover', label: '菜单栏' },
                        { value: 'panel', label: '桌面浮窗' },
                      ]}
                      onChange={() => void api.systemToggleMode()}
                    />
                  </Row>
                  <div style={{ height: 1, background: 'var(--divider)' }} />
                  <Row title="登录 Mac 时启动" detail="开机后，让 Doing 在手边。">
                    <Toggle checked={launchAtLogin} onChange={(v) => void setLogin(v)} />
                  </Row>
                  {launchMessage ? (
                    <>
                      <Notice message={launchMessage} isError />
                      <ActionButton onClick={() => void api.systemOpenSettings()}>
                        打开系统设置
                      </ActionButton>
                    </>
                  ) : null}
                </Group>
                <Group title="菜单栏">
                  <Row title="显示当前焦点" detail="不用打开面板，也知道正在做什么。">
                    <Toggle
                      checked={settings.showFocusInMenuBar}
                      onChange={(v) => patch({ showFocusInMenuBar: v })}
                    />
                  </Row>
                  <Row
                    title="最多显示字数"
                    control={
                      <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
                        <button
                          type="button"
                          disabled={!settings.showFocusInMenuBar}
                          onClick={() => patch({ menuBarTextLimit: Math.max(1, settings.menuBarTextLimit - 1) })}
                          style={{ fontSize: 15, color: 'var(--text-dim)', padding: '2px 6px' }}
                        >
                          −
                        </button>
                        <span style={{ fontSize: 12, fontWeight: 600, minWidth: 34, textAlign: 'center' }}>
                          {settings.menuBarTextLimit} 字
                        </span>
                        <button
                          type="button"
                          disabled={!settings.showFocusInMenuBar}
                          onClick={() => patch({ menuBarTextLimit: Math.min(60, settings.menuBarTextLimit + 1) })}
                          style={{ fontSize: 15, color: 'var(--text-dim)', padding: '2px 6px' }}
                        >
                          +
                        </button>
                      </span>
                    }
                  />
                </Group>
                <div style={{ fontSize: 12, color: 'var(--text-dim)', paddingLeft: 2, lineHeight: 1.8 }}>
                  面板内快捷键：⌘N 新事项 · ⌘Z 撤销 · ⌘⇧Z 重做 · ⌘, 设置
                  <br />
                  任务列表中：↑↓ 选择，空格完成，回车编辑。
                </div>
              </>
            ) : null}

            {section === 'reminders' ? (
              <>
                <Group title="到期提醒">
                  <Row title="发送截止通知" detail="事项到期时，用系统通知提醒你。">
                    <Toggle
                      checked={settings.notificationsEnabled}
                      onChange={(v) => patch({ notificationsEnabled: v })}
                    />
                  </Row>
                  <Row title="通知声音">
                    <Toggle
                      checked={settings.notificationSound}
                      disabled={!settings.notificationsEnabled}
                      onChange={(v) => patch({ notificationSound: v })}
                    />
                  </Row>
                  <div style={{ height: 1, background: 'var(--divider)' }} />
                  <Row title="菜单栏显示逾期状态">
                    <Toggle
                      checked={settings.showOverdueInMenuBar}
                      onChange={(v) => patch({ showOverdueInMenuBar: v })}
                    />
                  </Row>
                  <Row title="面板显示逾期数量" detail="用简短提示代替大面积警告。">
                    <Toggle
                      checked={settings.showOverdueBanner}
                      onChange={(v) => patch({ showOverdueBanner: v })}
                    />
                  </Row>
                </Group>
                <Group title="临期提示">
                  <Row title="突出显示即将到期的事项">
                    <Toggle
                      checked={settings.dueSoonEnabled}
                      onChange={(v) => patch({ dueSoonEnabled: v })}
                    />
                  </Row>
                  <Row title="提前多久">
                    <select
                      value={settings.dueSoonHours}
                      disabled={!settings.dueSoonEnabled}
                      onChange={(e) => patch({ dueSoonHours: Number(e.target.value) })}
                      style={{
                        background: 'var(--surface)',
                        color: 'var(--text)',
                        border: '1px solid var(--stroke)',
                        borderRadius: 8,
                        padding: '6px 8px',
                        fontSize: 12,
                        width: 135,
                      }}
                    >
                      {DUE_SOON_OPTIONS.map((o) => (
                        <option key={o.value} value={o.value}>
                          {o.label}
                        </option>
                      ))}
                    </select>
                  </Row>
                  <div style={{ fontSize: 12, color: 'var(--text-dim)' }}>
                    只改变日期的视觉提示，不会额外发送通知。
                  </div>
                </Group>
                <Group title="系统权限">
                  <Row
                    title="macOS 通知权限"
                    control={
                      <span
                        style={{
                          fontSize: 12,
                          fontWeight: 600,
                          color:
                            notifGranted === false ? 'var(--danger)' : 'var(--text-dim)',
                        }}
                      >
                        {notifGranted === null ? '检查中…' : notifGranted ? '已允许' : '已拒绝'}
                      </span>
                    }
                  />
                </Group>
              </>
            ) : null}

            {section === 'account' ? (
              <>
                <Group title="Doing 账户">
                  <Row
                    title={auth?.loggedIn ? '账户已连接' : '尚未登录'}
                    detail={
                      auth?.loggedIn
                        ? auth.username
                          ? `${auth.username} · 在不同设备间延续你的事项。`
                          : '在不同设备间延续你的事项。'
                        : '登录后，将本地事项与账户同步。'
                    }
                    control={
                      auth?.loggedIn ? (
                        <ActionButton kind="quiet" onClick={() => setConfirmLogout(true)}>
                          退出登录
                        </ActionButton>
                      ) : null
                    }
                  />
                </Group>
                <Group title="云端同步">
                  <Row
                    title="自动同步"
                    detail="关闭后仍会保存在此设备，可手动同步。"
                    control={
                      <Toggle
                        checked={settings.automaticSync}
                        disabled={!auth?.loggedIn}
                        onChange={(v) => patch({ automaticSync: v })}
                      />
                    }
                  />
                  <div style={{ height: 1, background: 'var(--divider)' }} />
                  <Row
                    title="状态"
                    control={
                      <span style={{ fontSize: 12, fontWeight: 600, color: 'var(--text-dim)' }}>
                        {sync ? SYNC_TEXT[sync.state] : '—'}
                      </span>
                    }
                  />
                  {sync?.lastError ? <Notice message={sync.lastError} isError /> : null}
                  {sync?.lastSyncAt ? (
                    <div style={{ fontSize: 11, color: 'var(--text-dim)' }}>
                      最近成功：{new Date(sync.lastSyncAt).toLocaleString('zh-CN', { hour12: false })}
                    </div>
                  ) : null}
                  <div style={{ display: 'flex', gap: 8 }}>
                    <ActionButton
                      kind="primary"
                      disabled={!auth?.loggedIn || sync?.state === 'syncing' || sync?.state === 'conflict'}
                      onClick={() => void run(() => api.syncFlush())}
                    >
                      立即同步
                    </ActionButton>
                    {sync?.conflictCloudVersion != null ? (
                      <ActionButton onClick={() => void api.syncRestore()}>从云端恢复…</ActionButton>
                    ) : null}
                  </div>
                </Group>
              </>
            ) : null}

            {section === 'data' ? (
              <>
                <Group title="此设备的事项">
                  <Row
                    title="本地内容"
                    detail={`${activeCount} 件待办 · ${doneCount} 件已完成`}
                    control={<Icon name="drive" size={23} />}
                  />
                  <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
                    <ActionButton
                      kind="primary"
                      onClick={async () => {
                        const ok = await api.systemSaveNow()
                        setNotice({
                          kind: ok ? 'ok' : 'err',
                          text: ok ? '已保存到此设备。' : '保存失败，请检查数据目录权限。',
                        })
                      }}
                    >
                      立即保存
                    </ActionButton>
                    <ActionButton onClick={() => void api.systemRevealData()}>
                      在 Finder 中显示
                    </ActionButton>
                    <ActionButton
                      onClick={async () => {
                        try {
                          const path = await api.dataExportLegacy()
                          setNotice({ kind: 'ok', text: `已导出兼容旧版的数据：${path}` })
                        } catch (e) {
                          setNotice({ kind: 'err', text: e instanceof Error ? e.message : String(e) })
                        }
                      }}
                    >
                      导出兼容旧版（回滚用）…
                    </ActionButton>
                  </div>
                  {notice ? (
                    <Notice message={notice.text} isError={notice.kind === 'err'} symbol={notice.kind === 'ok' ? 'check' : 'exclamation'} />
                  ) : null}
                </Group>
                <Group title="关于 Doing">
                  <Row
                    title="Doing"
                    detail={version ? `版本 ${version}` : '开发版'}
                    control={<DoingMark size={42} />}
                  />
                  <div style={{ fontSize: 12, color: 'var(--text-dim)' }}>
                    一个轻巧的地方，放下惦记，专注眼前。
                  </div>
                </Group>
                <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
                  <span style={{ flex: 1, fontSize: 12, color: 'var(--text-dim)' }}>
                    只重置偏好，不删除事项或账户。
                  </span>
                  <ActionButton kind="secondary" onClick={() => setConfirmReset(true)}>
                    恢复默认设置…
                  </ActionButton>
                </div>
              </>
            ) : null}
          </div>
        </div>
      </div>

      {confirmReset ? (
        <ConfirmDialog
          title="恢复默认设置？"
          message="外观、提醒、打开方式等偏好将恢复默认值，事项和账户不会被删除。"
          confirmLabel="恢复设置"
          onConfirm={() => {
            setConfirmReset(false)
            void run(() => api.settingsReset())
          }}
          onCancel={() => setConfirmReset(false)}
        />
      ) : null}
      {confirmLogout ? (
        <ConfirmDialog
          title="退出当前账户？"
          message="此设备的事项不会被删除。重新登录后可继续使用。"
          confirmLabel="退出登录"
          onConfirm={() => {
            setConfirmLogout(false)
            void api.authLogout()
          }}
          onCancel={() => setConfirmLogout(false)}
        />
      ) : null}
    </div>
  )
}

function ConfirmDialog({
  title,
  message,
  confirmLabel,
  onConfirm,
  onCancel,
}: {
  title: string
  message: string
  confirmLabel: string
  onConfirm: () => void
  onCancel: () => void
}) {
  return (
    <div
      style={{
        position: 'fixed',
        inset: 0,
        zIndex: 300,
        background: 'rgba(20,16,28,0.35)',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
      }}
    >
      <div
        style={{
          background: 'var(--bg)',
          borderRadius: 16,
          boxShadow: 'var(--shadow-panel)',
          padding: 22,
          width: 340,
          display: 'flex',
          flexDirection: 'column',
          gap: 14,
        }}
      >
        <div style={{ fontSize: 17, fontWeight: 700, color: 'var(--text)' }}>{title}</div>
        <div style={{ fontSize: 12, color: 'var(--text-dim)', lineHeight: 1.6 }}>{message}</div>
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8 }}>
          <ActionButton kind="secondary" onClick={onCancel}>
            取消
          </ActionButton>
          <ActionButton kind="destructive" onClick={onConfirm}>
            {confirmLabel}
          </ActionButton>
        </div>
      </div>
    </div>
  )
}
