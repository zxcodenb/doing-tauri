import type { CommandError as CommandErrorView } from '../types'

/** 统一把 Tauri 的结构化错误变成 Error；组件不再把对象显示成 [object Object]。 */
export class CommandFailure extends Error {
  readonly code: string
  readonly retryable: boolean
  readonly currentVersion: string | null

  constructor(value: unknown) {
    const error = value as Partial<CommandErrorView> | null
    const message = typeof error?.message === 'string' ? error.message : typeof value === 'string' ? value : '操作失败，请重试'
    super(message)
    this.name = 'CommandFailure'
    this.code = typeof error?.code === 'string' ? error.code : 'operationFailed'
    this.retryable = error?.retryable === true
    this.currentVersion = typeof error?.currentVersion === 'string' ? error.currentVersion : null
  }
}
