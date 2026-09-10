// 通用 UI 原语（对应旧 Controls.swift）。

import { useState, type ButtonHTMLAttributes, type ReactNode } from 'react'
import { Icon, type IconName } from './icons'

export function IconButton({
  icon,
  help,
  active = false,
  size = 28,
  iconSize = 13,
  onAction,
}: {
  icon: IconName
  help: string
  active?: boolean
  size?: number
  iconSize?: number
  onAction: () => void
}) {
  const [hover, setHover] = useState(false)
  return (
    <button
      type="button"
      aria-label={help}
      title={help}
      onClick={onAction}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      style={{
        width: size,
        height: size,
        borderRadius: 7,
        display: 'inline-flex',
        alignItems: 'center',
        justifyContent: 'center',
        color: active ? 'var(--violet)' : hover ? 'var(--text)' : 'var(--text-dim)',
        background: hover || active ? 'var(--surface-raised)' : 'transparent',
      }}
    >
      <Icon name={icon} size={iconSize} />
    </button>
  )
}

type ActionKind = 'primary' | 'secondary' | 'quiet' | 'destructive'

export function ActionButton({
  kind = 'secondary',
  fullWidth = false,
  children,
  ...rest
}: { kind?: ActionKind; fullWidth?: boolean; children: ReactNode } & ButtonHTMLAttributes<HTMLButtonElement>) {
  const fg =
    kind === 'primary' ? 'var(--on-accent)' : kind === 'destructive' ? 'var(--danger)' : 'var(--text)'
  const bg =
    kind === 'primary'
      ? 'var(--accent)'
      : kind === 'secondary'
        ? 'var(--surface-raised)'
        : kind === 'destructive'
          ? 'var(--danger-soft)'
          : 'transparent'
  return (
    <button
      type="button"
      {...rest}
      style={{
        font: '600 12px/1.2 var(--font-ui, inherit)',
        color: fg,
        background: bg,
        padding: '0 13px',
        minHeight: 38,
        borderRadius: 10,
        width: fullWidth ? '100%' : undefined,
        display: 'inline-flex',
        alignItems: 'center',
        justifyContent: 'center',
        gap: 7,
        opacity: rest.disabled ? 0.45 : 1,
        cursor: rest.disabled ? 'default' : 'pointer',
        ...rest.style,
      }}
    >
      {children}
    </button>
  )
}

export function FieldBox({
  focused,
  children,
}: {
  focused: boolean
  children: ReactNode
}) {
  return (
    <div
      style={{
        background: 'var(--surface)',
        border: `${focused ? 2 : 1}px solid ${focused ? 'var(--violet)' : 'var(--stroke)'}`,
        borderRadius: 10,
        minHeight: 42,
        display: 'flex',
        alignItems: 'center',
        padding: '0 12px',
      }}
    >
      {children}
    </div>
  )
}

export function Notice({
  message,
  isError = false,
  symbol,
  style,
}: {
  message: string
  isError?: boolean
  symbol?: IconName
  style?: React.CSSProperties
}) {
  return (
    <div
      role={isError ? 'alert' : 'status'}
      style={{
        display: 'flex',
        alignItems: 'flex-start',
        gap: 8,
        padding: 11,
        borderRadius: 9,
        background: isError ? 'var(--danger-soft)' : 'var(--surface-raised)',
        color: isError ? 'var(--danger)' : 'var(--text-dim)',
        fontSize: 12,
        ...style,
      }}
    >
      <Icon name={symbol ?? (isError ? 'exclamation' : 'sparkle')} size={13} />
      <span style={{ lineHeight: 1.4 }}>{message}</span>
    </div>
  )
}

export function CountBadge({ count }: { count: number }) {
  return (
    <span
      style={{
        fontSize: 11,
        color: 'var(--text-dim)',
        background: 'var(--surface-raised)',
        padding: '2px 7px',
        borderRadius: 999,
        fontVariantNumeric: 'tabular-nums',
      }}
    >
      {count}
    </span>
  )
}

export function SurfaceCard({ children }: { children: ReactNode }) {
  return (
    <div
      style={{
        background: 'var(--surface)',
        border: '1px solid var(--divider)',
        borderRadius: 14,
        padding: 16,
        width: '100%',
      }}
    >
      {children}
    </div>
  )
}

/** 品牌标记（doing mark）。 */
export function DoingMark({ size = 22 }: { size?: number }) {
  return (
    <span
      aria-hidden
      style={{
        display: 'inline-flex',
        width: size,
        height: size,
        borderRadius: size * 0.29,
        background: 'var(--accent)',
        color: 'var(--on-accent)',
        alignItems: 'center',
        justifyContent: 'center',
        flex: 'none',
      }}
    >
      <Icon name="check" size={size * 0.5} strokeWidth={2.6} />
    </span>
  )
}

export function Wordmark() {
  return (
    <span style={{ display: 'inline-flex', alignItems: 'center', gap: 7, color: 'var(--text)' }}>
      <DoingMark size={22} />
      <span
        style={{
          font: '700 16px "SF Pro Rounded", -apple-system, sans-serif',
          letterSpacing: -0.6,
        }}
      >
        doing
      </span>
    </span>
  )
}

/** 顶部圆角裁剪与背景。 */
export function PanelSurface({
  children,
  rounded = 12,
  panelMode = false,
}: {
  children: ReactNode
  rounded?: number
  panelMode?: boolean
}) {
  return (
    <div
      style={{
        height: '100%',
        background: 'var(--bg)',
        borderRadius: rounded,
        overflow: 'hidden',
        display: 'flex',
        flexDirection: 'column',
        boxShadow: panelMode ? 'var(--shadow-panel)' : 'none',
      }}
    >
      {children}
    </div>
  )
}
