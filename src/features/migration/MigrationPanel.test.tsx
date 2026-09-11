import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MigrationPanel } from './MigrationPanel'
import type { MigrationStatus } from '../../types'

const calls = vi.hoisted(() => ({ import: vi.fn(), resume: vi.fn(), cancel: vi.fn(), keep: vi.fn(), reveal: vi.fn() }))
vi.mock('../../lib/ipc', () => ({ api: { migrationImport: calls.import, migrationResume: calls.resume, migrationCancel: calls.cancel, migrationKeepCurrent: calls.keep, systemRevealData: calls.reveal } }))
vi.mock('../../hooks/useDoing', () => ({ useDoing: () => ({ run: async (action: () => Promise<unknown>) => { await action(); return { ok: true, message: '' } } }) }))
const status = (overrides: Partial<MigrationStatus> = {}): MigrationStatus => ({
  eventRevision: 1, available: true, detectedFile: '/fixture/items.json', imported: false, error: null, backupPath: null,
  transactionId: null, recoveryRequired: false, preferencesAvailable: true, importedPreferences: false, requiresLogin: false, warnings: [], ...overrides,
})
afterEach(() => { cleanup(); vi.resetAllMocks() })

describe('迁移确认与恢复', () => {
  it('只有确认后才导入；旧偏好替换有单独且明确的选项', async () => {
    const user = userEvent.setup(); render(<MigrationPanel status={status()} />)
    await user.click(screen.getByRole('button', { name: '导入' }))
    expect(calls.import).not.toHaveBeenCalled()
    expect(screen.getByRole('dialog', { name: '确认导入旧版数据' })).toBeInTheDocument()
    expect(screen.getByRole('checkbox')).toBeChecked()
    expect(screen.getByRole('button', { name: '返回' })).toHaveFocus()
    await user.click(screen.getByRole('button', { name: '确认导入' }))
    expect(calls.import).toHaveBeenCalledWith('/fixture/items.json', true)
  })
  it('用户可以只导入任务，不改变本机偏好', async () => {
    const user = userEvent.setup(); render(<MigrationPanel status={status()} />)
    await user.click(screen.getByRole('button', { name: '导入' }))
    await user.click(screen.getByRole('checkbox'))
    await user.click(screen.getByRole('button', { name: '确认导入' }))
    expect(calls.import).toHaveBeenCalledWith('/fixture/items.json', false)
  })
  it('返回或 Esc 取消确认不会发出迁移写命令', async () => {
    const user = userEvent.setup(); render(<MigrationPanel status={status()} />)
    const trigger = screen.getByRole('button', { name: '导入' })
    await user.click(trigger); await user.keyboard('{Escape}')
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument(); expect(trigger).toHaveFocus()
    expect(calls.import).not.toHaveBeenCalled()
  })
  it('恢复使用当前事务 ID，撤销需要二次确认', async () => {
    const user = userEvent.setup(); const id = '11111111-1111-4111-8111-111111111111'
    render(<MigrationPanel status={status({ available: false, recoveryRequired: true, transactionId: id, error: '来源已变化' })} />)
    expect(screen.getByRole('alert')).toHaveTextContent('来源已变化')
    await user.click(screen.getByRole('button', { name: '继续恢复' }))
    await waitFor(() => expect(calls.resume).toHaveBeenCalledWith(id))
    await user.click(screen.getByRole('button', { name: '撤销本次导入' }))
    expect(calls.cancel).not.toHaveBeenCalled()
    await user.click(screen.getByRole('button', { name: '确认撤销' }))
    expect(calls.cancel).toHaveBeenCalledWith(id)
  })
  it('保留当前文件不被描述为迁移完成，且需明确确认', async () => {
    const user = userEvent.setup(); const id = '22222222-2222-4222-8222-222222222222'
    render(<MigrationPanel status={status({ available: false, recoveryRequired: true, transactionId: id })} />)
    await user.click(screen.getByRole('button', { name: '保留当前文件' }))
    expect(calls.keep).not.toHaveBeenCalled()
    expect(screen.getByRole('dialog')).toHaveTextContent('这不会标记迁移成功')
    await user.click(screen.getByRole('button', { name: '确认保留当前文件' }))
    expect(calls.keep).toHaveBeenCalledWith(id)
  })
  it('不可识别的日志不能盲目恢复、撤销或保留，但可定位数据目录', async () => {
    const user = userEvent.setup(); render(<MigrationPanel status={status({ available: false, recoveryRequired: true, transactionId: null, error: '日志版本不支持' })} />)
    for (const name of ['继续恢复', '撤销本次导入', '保留当前文件']) expect(screen.getByRole('button', { name })).toBeDisabled()
    await user.click(screen.getByRole('button', { name: '打开数据目录' })); expect(calls.reveal).toHaveBeenCalledTimes(1)
  })
  it('新的事务替换旧事务时，旧确认选择不会继续提交', async () => {
    const user = userEvent.setup(); const one = status({ available: false, recoveryRequired: true, transactionId: 'old' })
    const { rerender } = render(<MigrationPanel key={one.transactionId} status={one} />)
    await user.click(screen.getByRole('button', { name: '撤销本次导入' }))
    const two = { ...one, transactionId: 'new' }; rerender(<MigrationPanel key={two.transactionId} status={two} />)
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument(); expect(calls.cancel).not.toHaveBeenCalled()
  })
})
