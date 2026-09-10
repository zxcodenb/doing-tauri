// DueStamp/预设 语义测试（对照旧 Swift DueStampTests；运行于 TZ=Asia/Shanghai）。
import { describe, expect, it } from 'vitest'
import { DUE_PRESETS, dueStampText, nextHour, presetDate } from './time'

function at(y: number, m: number, d: number, hh: number, mm = 0): Date {
  return new Date(y, m - 1, d, hh, mm, 0, 0)
}

const NOW = at(2026, 9, 3, 10, 0) // 2026-09-03 周四 10:00（Asia/Shanghai）

describe('dueStampText（相对日期文案）', () => {
  it('当天显示时间', () => {
    expect(dueStampText(at(2026, 9, 3, 14, 0).toISOString(), NOW)).toBe('14:00')
  })
  it('明天显示“明天 HH:mm”', () => {
    expect(dueStampText(at(2026, 9, 4, 9, 30).toISOString(), NOW)).toBe('明天 09:30')
  })
  it('昨天显示“昨天 HH:mm”', () => {
    expect(dueStampText(at(2026, 9, 2, 18, 0).toISOString(), NOW)).toBe('昨天 18:00')
  })
  it('2-6 天内显示短星期', () => {
    // 2026-09-07 是周一
    expect(dueStampText(at(2026, 9, 7, 14, 0).toISOString(), NOW)).toBe('周一 14:00')
  })
  it('同年其他日期显示 M/d HH:mm', () => {
    expect(dueStampText(at(2026, 9, 12, 14, 0).toISOString(), NOW)).toBe('9/12 14:00')
    expect(dueStampText(at(2026, 8, 20, 14, 0).toISOString(), NOW)).toBe('8/20 14:00')
  })
  it('跨年显示完整日期', () => {
    expect(dueStampText(at(2027, 1, 5, 9, 0).toISOString(), NOW)).toBe('2027/1/5')
  })
})

describe('截止预设（跨午夜与 DST 保持未来）', () => {
  it('所有预设都晚于当前时刻（深夜场景）', () => {
    const late = at(2026, 3, 7, 23, 50)
    for (const p of DUE_PRESETS) {
      expect(presetDate(p.kind, late).getTime()).toBeGreaterThan(late.getTime())
    }
  })
  it('明天上午固定为 09:00', () => {
    const late = at(2026, 3, 7, 23, 50)
    const t = presetDate('tomorrow', late)
    expect(t.getDate()).toBe(8)
    expect(t.getHours()).toBe(9)
    expect(t.getMinutes()).toBe(0)
  })
  it('nextHour 为下一整点', () => {
    const x = nextHour(at(2026, 9, 3, 10, 0))
    expect(x.getHours()).toBe(11)
    expect(x.getMinutes()).toBe(0)
  })
})
