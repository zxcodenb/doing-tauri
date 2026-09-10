import type { JSX } from 'react'

// 统一矢量图标（自行绘制，许可安全；stroke 风格近似 SF Symbols 基础形）。
type IconName =
  | 'check'
  | 'plus'
  | 'x'
  | 'calendar'
  | 'clock'
  | 'dots'
  | 'pin'
  | 'pinFill'
  | 'scope'
  | 'chevronDown'
  | 'chevronRight'
  | 'trash'
  | 'pencil'
  | 'exclamation'
  | 'cloud'
  | 'arrowRetry'
  | 'sparkle'
  | 'branch'
  | 'person'
  | 'personBadge'
  | 'gear'
  | 'bell'
  | 'drive'
  | 'arrowRight'
  | 'icloud'
  | 'checkIcloud'
  | 'clockCircle'
  | 'link'

const PATHS: Record<IconName, JSX.Element> = {
  check: <path d="M5 12.5l4.5 4.5L19 7.5" />,
  plus: <path d="M12 5v14M5 12h14" />,
  x: <path d="M6 6l12 12M18 6L6 18" />,
  calendar: (
    <>
      <rect x="3.5" y="5.5" width="17" height="15" rx="2.5" />
      <path d="M3.5 10.5h17M8 3.5v4M16 3.5v4" />
    </>
  ),
  clock: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 7.5V12l3 2" />
    </>
  ),
  dots: (
    <>
      <circle cx="5.5" cy="12" r="1.2" fill="currentColor" stroke="none" />
      <circle cx="12" cy="12" r="1.2" fill="currentColor" stroke="none" />
      <circle cx="18.5" cy="12" r="1.2" fill="currentColor" stroke="none" />
    </>
  ),
  pin: <path d="M9.5 4.5h5L12 7l2.5 5.5h-5L12 7zM12 7v11" />,
  pinFill: <path d="M9.5 4.5h5L12 7l2.5 5.5h-5L12 7zM12 12.5V18" fill="currentColor" />,
  scope: (
    <>
      <circle cx="12" cy="12" r="3" />
      <circle cx="12" cy="12" r="8" />
      <path d="M12 2v4M12 18v4M2 12h4M18 12h4" />
    </>
  ),
  chevronDown: <path d="M5.5 9l6.5 6.5L18.5 9" />,
  chevronRight: <path d="M9 5.5l6.5 6.5L9 18.5" />,
  trash: (
    <>
      <path d="M4.5 6.5h15M9.5 6.5v-2h5v2M6.5 6.5l1 14h9l1-14" />
      <path d="M10 10.5v6M14 10.5v6" />
    </>
  ),
  pencil: <path d="M4 20l1-4.5L16.5 4a1.8 1.8 0 012.5 2.5L7.5 18.5 4 20z" />,
  exclamation: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 7.5V13" />
      <circle cx="12" cy="16" r="1" fill="currentColor" stroke="none" />
    </>
  ),
  cloud: <path d="M7 17.5h10a4 4 0 00.8-7.9A5.5 5.5 0 007.2 11 3.8 3.8 0 007 17.5z" />,
  arrowRetry: (
    <>
      <path d="M19.5 12a7.5 7.5 0 11-2.2-5.3" />
      <path d="M19.8 3.5v4h-4" />
    </>
  ),
  sparkle: <path d="M12 3l2 6 6 2-6 2-2 6-2-6-6-2 6-2z" />,
  branch: (
    <>
      <circle cx="6.5" cy="5.5" r="2.2" />
      <circle cx="6.5" cy="18.5" r="2.2" />
      <circle cx="17.5" cy="12" r="2.2" />
      <path d="M6.5 7.7v8.6M8.7 12h6.6" />
    </>
  ),
  person: (
    <>
      <circle cx="12" cy="8" r="3.6" />
      <path d="M4.8 20a7.2 7.2 0 0114.4 0" />
    </>
  ),
  personBadge: (
    <>
      <circle cx="12" cy="8" r="3.6" />
      <path d="M4.8 20a7.2 7.2 0 0114.4 0" />
      <path d="M18.5 15v5M16 17.5h5" />
    </>
  ),
  gear: (
    <>
      <circle cx="12" cy="12" r="3.2" />
      <path d="M12 2.8l1.6 2.6 3-.4 1 2.9 2.9 1-.4 3 2.6 1.6-2.6 1.6.4 3-2.9 1-1 2.9-3-.4-1.6 2.6-1.6-2.6-3 .4-1-2.9-2.9-1 .4-3L2.8 12l2.6-1.6-.4-3 2.9-1 1-2.9 3 .4z" />
    </>
  ),
  bell: (
    <>
      <path d="M6 16.5V11a6 6 0 0112 0v5.5l2 2.5H4z" />
      <path d="M10 21a2.2 2.2 0 004 0" />
    </>
  ),
  drive: (
    <>
      <rect x="3" y="6.5" width="18" height="11" rx="2.5" />
      <path d="M7.5 12h.01M11.5 12h.01" />
    </>
  ),
  arrowRight: <path d="M4.5 12h15M13.5 6l6 6-6 6" />,
  icloud: <path d="M7 17.5h10a4 4 0 00.8-7.9A5.5 5.5 0 007.2 11 3.8 3.8 0 007 17.5z" />,
  checkIcloud: (
    <>
      <path d="M7 17.5h10a4 4 0 00.8-7.9A5.5 5.5 0 007.2 11 3.8 3.8 0 007 17.5z" />
      <path d="M8.8 13.8l2.2 2.2 4-4.4" />
    </>
  ),
  clockCircle: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 7.5V12l3 2" />
    </>
  ),
  link: (
    <>
      <path d="M9.5 14.5l5-5" />
      <path d="M7 11.5L5.5 13a4 4 0 005.7 5.7L12.7 17" />
      <path d="M17 12.5l1.5-1.5a4 4 0 00-5.7-5.7L11.3 7" />
    </>
  ),
}

export function Icon({
  name,
  size = 14,
  strokeWidth = 1.8,
  className,
}: {
  name: IconName
  size?: number
  strokeWidth?: number
  className?: string
}) {
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      {PATHS[name]}
    </svg>
  )
}

export type { IconName }
