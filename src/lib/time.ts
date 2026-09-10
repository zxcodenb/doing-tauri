// 时间工具：截止文案/预设，与 Swift DueStamp/DuePreset 语义一致（日历日、本地时区）。

export const WEEKDAYS_ZH = ['周日', '周一', '周二', '周三', '周四', '周五', '周六']

export interface CalendarView {
  startOfDay(d: Date): Date
  addDays(d: Date, n: number): Date
  isSameYear(a: Date, b: Date): boolean
}

export const systemCalendar: CalendarView = {
  startOfDay(d) {
    const x = new Date(d)
    x.setHours(0, 0, 0, 0)
    return x
  },
  addDays(d, n) {
    const x = new Date(d)
    x.setDate(x.getDate() + n)
    return x
  },
  isSameYear(a, b) {
    return a.getFullYear() === b.getFullYear()
  },
}

function pad(n: number): string {
  return String(n).padStart(2, '0')
}

function hm(d: Date): string {
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`
}

/** DueStamp.text：当天 14:00 / 明天 09:30 / 昨天 18:00 / 周内 周一 14:00 / 同年 9/12 14:00 / 跨年 2027/1/5。 */
export function dueStampText(due: string, now: Date, cal: CalendarView = systemCalendar): string {
  const dueDate = new Date(due)
  const startDue = cal.startOfDay(dueDate).getTime()
  const startNow = cal.startOfDay(now).getTime()
  const days = Math.round((startDue - startNow) / 86_400_000)
  const time = hm(dueDate)
  switch (true) {
    case days === 0:
      return time
    case days === 1:
      return `明天 ${time}`
    case days === -1:
      return `昨天 ${time}`
    case days >= 2 && days <= 6:
      return `${WEEKDAYS_ZH[dueDate.getDay()]} ${time}`
    default:
      if (cal.isSameYear(dueDate, now)) {
        return `${dueDate.getMonth() + 1}/${dueDate.getDate()} ${time}`
      }
      return `${dueDate.getFullYear()}/${dueDate.getMonth() + 1}/${dueDate.getDate()}`
  }
}

export type DuePresetKind = 'quarterHour' | 'hour' | 'threeHours' | 'tomorrow'

export const DUE_PRESETS: { kind: DuePresetKind; title: string }[] = [
  { kind: 'quarterHour', title: '15 分钟后' },
  { kind: 'hour', title: '1 小时后' },
  { kind: 'threeHours', title: '3 小时后' },
  { kind: 'tomorrow', title: '明天上午' },
]

export function presetDate(kind: DuePresetKind, now: Date): Date {
  switch (kind) {
    case 'quarterHour':
      return new Date(now.getTime() + 15 * 60_000)
    case 'hour':
      return new Date(now.getTime() + 60 * 60_000)
    case 'threeHours':
      return new Date(now.getTime() + 3 * 60 * 60_000)
    case 'tomorrow': {
      const day = new Date(now)
      day.setDate(day.getDate() + 1)
      day.setHours(9, 0, 0, 0)
      return day
    }
  }
}

/** 默认的“下一整点”（不含当前整点自身）。 */
export function nextHour(now: Date): Date {
  const x = new Date(now)
  x.setHours(x.getHours() + 1, 0, 0, 0)
  return x
}

/** 本地日期输入（yyyy-MM-dd）与时间输入（HH:mm）→ Date。 */
export function combineLocal(dateText: string, timeText: string): Date {
  const [y, m, d] = dateText.split('-').map(Number)
  const [hh, mm] = timeText.split(':').map(Number)
  return new Date(y, m - 1, d, hh || 0, mm || 0, 0, 0)
}

export function formatDateInput(d: Date): string {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

export function formatTimeInput(d: Date): string {
  return hm(d)
}

export function toIsoLocal(d: Date): string {
  return d.toISOString()
}

export function fmtShortDateTime(iso: string | null): string {
  if (!iso) return ''
  const d = new Date(iso)
  const now = new Date()
  const sameDay = systemCalendar.startOfDay(d).getTime() === systemCalendar.startOfDay(now).getTime()
  if (sameDay) return `今天 ${hm(d)}`
  return `${d.getMonth() + 1}月${d.getDate()}日 ${hm(d)}`
}
