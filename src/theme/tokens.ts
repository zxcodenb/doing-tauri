// 设计参数（源自旧 Swift Theme.swift 的设计令牌；颜色由 CSS 变量承载）。

export const size = {
  width: 380,
  height: 580,
  settingsWidth: 760,
  settingsHeight: 610,
  radius: 20,
  padding: 20,
  rowRadius: 10,
  controlRadius: 10,
  fieldHeight: 42,
  buttonHeight: 38,
  markerSize: 18,
} as const

export const font = {
  display: "'SF Pro Rounded', -apple-system, 'PingFang SC', 'Helvetica Neue', sans-serif",
  ui: "-apple-system, BlinkMacSystemFont, 'PingFang SC', 'Helvetica Neue', sans-serif",
  mono: "'SF Mono', ui-monospace, Menlo, monospace",
} as const

export type DueStateKind = 'none' | 'upcoming' | 'dueSoon' | 'overdue'

export function classifyDue(
  due: string | null | undefined,
  done: boolean,
  now: Date,
  dueSoonEnabled: boolean,
  dueSoonHours: number,
): DueStateKind {
  if (!due || done) return 'none'
  const t = new Date(due).getTime()
  if (t <= now.getTime()) return 'overdue'
  if (dueSoonEnabled && t - now.getTime() <= dueSoonHours * 3600_000) return 'dueSoon'
  return 'upcoming'
}
