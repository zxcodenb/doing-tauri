import { describe, expect, it } from 'vitest'
import { CommandFailure } from './errors'

describe('安全 IPC 错误协议', () => {
  it('保留机器码、可读文案、可重试性与完整 i64 字符串，不显示对象占位符', () => {
    const error = new CommandFailure({ code: 'snapshot_conflict', message: '云端已变化', retryable: false, currentVersion: '9007199254740993' })
    expect(error).toBeInstanceOf(Error)
    expect(error.message).toBe('云端已变化')
    expect(error.code).toBe('snapshot_conflict')
    expect(error.retryable).toBe(false)
    expect(error.currentVersion).toBe('9007199254740993')
  })
  it('兼容框架级的字符串拒绝；对未知对象只显示安全兜底', () => {
    expect(new CommandFailure('窗口已关闭').message).toBe('窗口已关闭')
    expect(new CommandFailure({ privateToken: 'private' }).message).toBe('操作失败，请重试')
    expect(new CommandFailure({ message: '暂时离线', retryable: true }).retryable).toBe(true)
  })
})
