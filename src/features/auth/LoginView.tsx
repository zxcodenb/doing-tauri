// 登录/注册视图（LoginView 语义）：校验、回车提交、重复提交保护。

import { useRef, useState } from 'react'
import { api } from '../../lib/ipc'
import { useDoing } from '../../hooks/useDoing'
import { ActionButton, FieldBox, Notice } from '../../components/ui'
import { Icon } from '../../components/icons'

function canSubmit(username: string, password: string, registering: boolean): boolean {
  return (
    username.trim().length > 0 && (registering ? password.length >= 8 : password.length > 0)
  )
}

export function LoginView() {
  const { auth } = useDoing()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [registering, setRegistering] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const passwordRef = useRef<HTMLInputElement>(null)

  const displayError = error ?? auth?.error

  const submit = async () => {
    if (busy || !canSubmit(username, password, registering)) return
    setBusy(true)
    setError(null)
    try {
      if (registering) {
        await api.authRegister(username, password)
      } else {
        await api.authLogin(username, password)
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div style={{ flex: 1, overflowY: 'auto', padding: '4px 28px 24px' }}>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 26 }}>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 10, paddingTop: 14 }}>
          <span style={{ fontSize: 11, letterSpacing: 1.3, color: 'var(--violet)', fontWeight: 600 }}>
            A LITTLE LESS TO DO ✦
          </span>
          <span
            style={{
              fontSize: 28,
              fontWeight: 800,
              lineHeight: 1.25,
              letterSpacing: -0.7,
              color: 'var(--text)',
            }}
          >
            少一点惦记。
            <br />
            多一点完成。
          </span>
          <span style={{ fontSize: 12, color: 'var(--text-dim)' }}>
            {registering ? '创建账户，把想做的事留在手边。' : '登录 Doing，接着做你的下一件事。'}
          </span>
        </div>

        <div style={{ display: 'flex', flexDirection: 'column', gap: 15, opacity: busy ? 0.7 : 1 }}>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
            <span style={{ fontSize: 12, fontWeight: 600, color: 'var(--text)' }}>用户名</span>
            <FieldBox focused={false}>
              <input
                value={username}
                autoFocus
                onChange={(e) => setUsername(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && passwordRef.current?.focus()}
                placeholder="你的用户名"
                style={{ flex: 1, border: 'none', outline: 'none', background: 'transparent', fontSize: 14, color: 'var(--text)' }}
              />
            </FieldBox>
          </label>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
            <span style={{ display: 'flex', justifyContent: 'space-between' }}>
              <span style={{ fontSize: 12, fontWeight: 600, color: 'var(--text)' }}>密码</span>
              {registering ? (
                <span style={{ fontSize: 11, color: 'var(--text-dim)' }}>至少 8 位</span>
              ) : null}
            </span>
            <FieldBox focused={false}>
              <input
                ref={passwordRef}
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && !e.nativeEvent.isComposing && void submit()}
                placeholder={registering ? '设置一个密码' : '输入密码'}
                style={{ flex: 1, border: 'none', outline: 'none', background: 'transparent', fontSize: 14, color: 'var(--text)' }}
              />
            </FieldBox>
          </label>
        </div>

        {displayError ? (
          <Notice message={displayError} isError />
        ) : null}

        <ActionButton
          kind="primary"
          fullWidth
          disabled={busy || !canSubmit(username, password, registering)}
          onClick={() => void submit()}
        >
          {busy ? (
            '正在连接…'
          ) : registering ? (
            <>
              <Icon name="arrowRight" size={13} /> 创建账户，开始记录
            </>
          ) : (
            '登录'
          )}
        </ActionButton>

        <div style={{ display: 'flex', justifyContent: 'center', gap: 4, fontSize: 12 }}>
          <span style={{ color: 'var(--text-dim)' }}>
            {registering ? '已经有账户？' : '第一次使用 Doing？'}
          </span>
          <button
            type="button"
            disabled={busy}
            onClick={() => {
              setRegistering((v) => !v)
              setError(null)
            }}
            style={{ color: 'var(--violet)', fontWeight: 600 }}
          >
            {registering ? '去登录' : '创建账户'}
          </button>
        </div>
        <div style={{ display: 'flex', justifyContent: 'center', fontSize: 11, color: 'var(--text-faint)' }}>
          任务保存在此设备，并同步到你的账户。
        </div>
      </div>
    </div>
  )
}
