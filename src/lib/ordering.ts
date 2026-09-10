// 排序与展示规则：焦点唯一、计划优先稳定排序、手工顺序随后、完成折叠。
import type { ItemView } from '../types'

export interface Ordering {
  focus: ItemView | null
  remaining: ItemView[]
  completed: ItemView[]
}

export function orderItems(items: ItemView[], focusId: string | null): Ordering {
  const focus = focusId ? items.find((i) => i.id === focusId && !i.done) ?? null : null
  const active = items.filter((i) => !i.done && i.id !== focus?.id)
  const order = new Map(items.map((i, idx) => [i.id, idx]))
  const scheduled = active
    .filter((i) => i.dueDate)
    .sort((a, b) => {
      const da = new Date(a.dueDate!).getTime()
      const db = new Date(b.dueDate!).getTime()
      if (da === db) return order.get(a.id)! - order.get(b.id)!
      return da - db
    })
  const manual = active.filter((i) => !i.dueDate)
  return { focus, remaining: [...scheduled, ...manual], completed: items.filter((i) => i.done) }
}

export function isOverdueNow(items: ItemView[], now: Date): boolean {
  const t = now.getTime()
  return items.some((i) => !i.done && i.dueDate && new Date(i.dueDate).getTime() <= t)
}

export function countOverdue(items: ItemView[], now: Date): number {
  const t = now.getTime()
  return items.filter((i) => !i.done && i.dueDate && new Date(i.dueDate).getTime() <= t).length
}

export function activeCount(items: ItemView[]): number {
  return items.filter((i) => !i.done).length
}
