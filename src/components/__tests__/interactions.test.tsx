// 交互层组件测试（计划 §10.1“前端测试”：表单/编辑草稿/日期组件），
// 对应旧版 37 项原生交互检查的可自动化子集。
// 通过 mock Tauri IPC，在 jsdom 中以真实 DoingProvider + 组件渲染驱动。
import { describe, expect, it, vi } from 'vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach } from 'vitest'

const invokeMock = vi.fn()
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...args: unknown[]) => invokeMock(...args) }))

// 可注入的事件总线：捕获 Provider/App 注册的 doing:// 监听器，供测试回放 Rust 侧事件。
const eventBus = vi.hoisted(() => {
  const map = new Map<string, Set<(e: { payload: unknown }) => void>>()
  return {
    add(event: string, h: (e: { payload: unknown }) => void) {
      if (!map.has(event)) map.set(event, new Set())
      map.get(event)!.add(h)
    },
    remove(event: string, h: (e: { payload: unknown }) => void) {
      map.get(event)?.delete(h)
    },
    emit(event: string, payload: unknown) {
      for (const h of map.get(event) ?? []) h({ payload })
    },
    clear() {
      map.clear()
    },
  }
})
vi.mock('@tauri-apps/api/event', () => ({
  listen: async (event: string, handler: (e: { payload: unknown }) => void) => {
    eventBus.add(event, handler)
    return () => eventBus.remove(event, handler)
  },
}))
vi.mock('@tauri-apps/api/app', () => ({ getVersion: async () => '0.4.0' }))
const windowState = vi.hoisted(() => ({ label: 'main' }))
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ label: windowState.label, onFocusChanged: async (handler: (event: { payload: unknown }) => void) => { eventBus.add('test://native-focus', handler); return () => eventBus.remove('test://native-focus', handler) } }),
}))

import { DoingProvider } from '../../hooks/useDoing'
import { Workspace } from '../../features/workspace/Workspace'
import { InlineEditor } from '../../features/workspace/TaskRow'
import { DuePicker } from '../../components/DuePicker'
import { ConflictOverlay } from '../../features/workspace/ConflictOverlay'
import { SyncBadge } from '../../features/workspace/SyncBadge'
import { LoginView } from '../../features/auth/LoginView'
import { SettingsApp } from '../../features/settings/SettingsApp'
import { App } from '../../App'
import type { StartupView } from '../../types'
import { CANDIDATE_ID, startupFixture } from '../../test/fixtures'

const ITEM_ID = '11111111-1111-1111-1111-111111111111'

function startup(): StartupView {
  return {
    auth: { sessionGeneration: 1, eventRevision: 1, loggedIn: true, username: 'tester', serverUrl: 'http://127.0.0.1:8080', isAuthenticating: false, error: null },
    snapshot: {
      sessionGeneration: 1, eventRevision: 1,
      revision: 1,
      items: [
        {
          id: ITEM_ID,
          text: '写周报',
          done: false,
          createdAt: '2026-09-10T00:00:00Z',
          dueDate: null,
          updatedAt: '2026-09-10T00:00:00Z',
        },
      ],
      focusId: ITEM_ID,
      undoTitle: null,
      redoTitle: null,
      notifiedDueIds: [],
      saveFailed: false,
    },
    settings: {
      revision: 1, eventRevision: 1, error: null,
      appearance: 'system',
      mode: 'popover',
      showFocusInMenuBar: true,
      menuBarTextLimit: 18,
      notificationsEnabled: true,
      notificationSound: true,
      dueSoonEnabled: true,
      dueSoonHours: 24,
      showOverdueInMenuBar: true,
      showOverdueBanner: true,
      automaticSync: true,
    },
    sync: { sessionGeneration: 1, eventRevision: 1, conflictId: null, state: 'idle', lastSyncAt: null, lastError: null, conflictCloudCount: null, conflictCloudVersion: null },
    conflict: null,
    legacyImportAvailable: false,
    migration: null,
  }
}

const SECOND_ID = '22222222-2222-2222-2222-222222222222'
const DONE_ID = '33333333-3333-3333-3333-333333333333'

function extraItem(id: string, text: string, done = false) {
  return {
    id,
    text,
    done,
    createdAt: '2026-09-10T00:00:00Z',
    dueDate: null,
    updatedAt: '2026-09-10T00:00:00Z',
  }
}

/** 焦点=写周报、待办=买牛奶、已完成=已完成的事项（用于导航/折叠用例）。 */
function navStartup(): StartupView {
  const s = startup()
  return {
    ...s,
    snapshot: {
      ...s.snapshot,
      items: [...s.snapshot.items, extraItem(SECOND_ID, '买牛奶'), extraItem(DONE_ID, '已完成的事项', true)],
    },
  }
}

function installInvoke(overrides: Record<string, unknown> = {}) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd in overrides) {
      const v = overrides[cmd]
      return typeof v === 'function' ? (v as () => unknown)() : v
    }
    switch (cmd) {
      case 'init_state':
        return startup()
      case 'task_add':
        return { message: '已添加事项', offersUndo: true, id: ITEM_ID }
      case 'task_toggle_done':
        return { message: '完成了，又少一件事。', offersUndo: true, id: ITEM_ID }
      default:
        return { message: '', offersUndo: false, id: null }
    }
  })
}

afterEach(() => {
  cleanup()
  invokeMock.mockReset()
  vi.restoreAllMocks()
  vi.useRealTimers()
  Object.defineProperty(document, 'visibilityState', { value: 'visible', configurable: true })
  eventBus.clear()
  windowState.label = 'main'
  try {
    localStorage.clear()
  } catch {
    /* 忽略存储异常 */
  }
})

describe('工作区交互（表单与完成动作）', () => {
  it('录入回车触发 task_add，并展示反馈条', async () => {
    installInvoke()
    const user = userEvent.setup()
    render(
      <DoingProvider>
        <Workspace panelMode={false} />
      </DoingProvider>,
    )
    const input = await screen.findByLabelText('新事项')
    await user.type(input, '买牛奶{Enter}')
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('task_add', {
        arg: { text: '买牛奶' },
        due: null,
      })
    })
    expect(await screen.findByText('已添加事项')).toBeInTheDocument()
  })

  it('点击完成按钮触发 task_toggle_done', async () => {
    installInvoke()
    const user = userEvent.setup()
    render(
      <DoingProvider>
        <Workspace panelMode={false} />
      </DoingProvider>,
    )
    const toggle = await screen.findByLabelText('完成：写周报')
    await user.click(toggle)
    expect(invokeMock).toHaveBeenCalledWith('task_toggle_done', { id: ITEM_ID })
  })

  it('保存失败时展示错误提示（不清空已输入）', async () => {
    installInvoke({ task_add: () => Promise.reject('本地保存失败：磁盘只读') })
    const user = userEvent.setup()
    render(
      <DoingProvider>
        <Workspace panelMode={false} />
      </DoingProvider>,
    )
    const input = await screen.findByLabelText('新事项')
    await user.type(input, '保留我{Enter}')
    expect(await screen.findByRole('alert')).toHaveTextContent('本地保存失败：磁盘只读')
    expect((screen.getByLabelText('新事项') as HTMLInputElement).value).toBe('保留我')
  })

  it('切换窗口形态（菜单栏 ↔ 桌面浮窗）不丢草稿', async () => {
    installInvoke()
    const user = userEvent.setup()
    const { rerender } = render(
      <DoingProvider>
        <Workspace panelMode={false} />
      </DoingProvider>,
    )
    const input = await screen.findByLabelText('新事项')
    await user.type(input, '切换形态也不丢')
    rerender(
      <DoingProvider>
        <Workspace panelMode={true} />
      </DoingProvider>,
    )
    expect((screen.getByLabelText('新事项') as HTMLInputElement).value).toBe('切换形态也不丢')
  })
})

describe('行内编辑（草稿与保存）', () => {
  it('保存回传修剪后的文本并退出编辑', async () => {
    const user = userEvent.setup()
    const onSave = vi.fn()
    const onCancel = vi.fn()
    render(<InlineEditor initial="旧内容" isFocus={false} onSave={onSave} onCancel={onCancel} />)
    const area = screen.getByRole('textbox')
    await user.clear(area)
    await user.type(area, '  新内容  ')
    await user.click(screen.getByText('保存'))
    expect(onSave).toHaveBeenCalledWith('新内容')
  })

  it('空文本禁用保存', async () => {
    const user = userEvent.setup()
    const onSave = vi.fn()
    render(<InlineEditor initial="内容" isFocus={false} onSave={onSave} onCancel={() => {}} />)
    await user.clear(screen.getByRole('textbox'))
    const save = screen.getByText('保存') as HTMLButtonElement
    expect(save).toBeDisabled()
  })
})

describe('截止时间弹层（预设与移除）', () => {
  it('预设保存回传未来时间（≈15 分钟后）；未点保存不提交', async () => {
    const user = userEvent.setup()
    const onSave = vi.fn()
    render(<DuePicker initial={null} onSave={onSave} onClose={() => {}} />)
    await user.click(screen.getByText('15 分钟后'))
    expect(onSave).not.toHaveBeenCalled()
    await user.click(screen.getByText('保存时间'))
    expect(onSave).toHaveBeenCalledTimes(1)
    const iso = onSave.mock.calls[0][0] as string
    const delta = new Date(iso).getTime() - Date.now()
    expect(delta).toBeGreaterThan(14 * 60_000)
    expect(delta).toBeLessThan(16 * 60_000)
  })

  it('取消关闭不提交（数据不变）', async () => {
    const user = userEvent.setup()
    const onSave = vi.fn()
    const onClose = vi.fn()
    render(<DuePicker initial={new Date('2026-09-12T09:00:00Z')} onSave={onSave} onClose={onClose} />)
    await user.click(screen.getByText('取消'))
    expect(onSave).not.toHaveBeenCalled()
    expect(onClose).toHaveBeenCalled()
  })

  it('已有截止时间时可移除（回传 null）', async () => {
    const user = userEvent.setup()
    const onSave = vi.fn()
    render(<DuePicker initial={new Date('2026-09-12T09:00:00Z')} onSave={onSave} onClose={() => {}} />)
    await user.click(screen.getByText('移除'))
    expect(onSave).toHaveBeenCalledWith(null)
  })
})

describe('冲突面板（必须先选择后确认；暂缓不覆盖）', () => {
  function conflictStartup(): StartupView {
    const s = startup()
    return {
      ...s,
      snapshot: { ...s.snapshot, items: [...s.snapshot.items, {
        id: '22222222-2222-2222-2222-222222222222',
        text: '本地第二条',
        done: false,
        createdAt: '2026-09-10T00:00:00Z',
        dueDate: null,
        updatedAt: '2026-09-10T00:00:00Z',
      }] },
      sync: { ...s.sync, state: 'conflict', conflictCloudCount: 1, conflictCloudVersion: '9007199254740993', conflictId: CANDIDATE_ID },
      conflict: {
        sessionGeneration: 1, eventRevision: 1, candidateId: CANDIDATE_ID, reason: '本地与云端快照不同',
        cloudVersion: '9007199254740993',
        cloudCount: 1,
        cloudPreview: [],
        updatedAt: null,
      },
    }
  }

  it('未选择时确认按钮禁用；选择云端后携带候选版本提交', async () => {
    installInvoke({ init_state: () => conflictStartup(), conflict_choose_cloud: { message: '', offersUndo: false, id: null } })
    const user = userEvent.setup()
    render(
      <DoingProvider>
        <ConflictOverlay />
      </DoingProvider>,
    )
    const confirm = await screen.findByText('选择后继续')
    expect(confirm).toBeDisabled()
    await user.click(screen.getByText('云端的事项'))
    const useCloud = await screen.findByText('使用云端')
    expect(useCloud).not.toBeDisabled()
    await user.click(useCloud)
    expect(invokeMock).toHaveBeenCalledWith('conflict_choose_cloud', { arg: { candidateId: CANDIDATE_ID, cloudVersion: '9007199254740993' } })
  })

  it('候选 ID 变更后清除旧选择，即使版本字符串相同也必须重新确认', async () => {
    installInvoke({ init_state: () => conflictStartup() })
    const user = userEvent.setup()
    render(<DoingProvider><ConflictOverlay /></DoingProvider>)
    await user.click(await screen.findByText('云端的事项'))
    const next = { ...conflictStartup().conflict!, eventRevision: 3, candidateId: 'bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb' }
    act(() => eventBus.emit('doing://conflict', next))
    expect(screen.getByText('选择后继续')).toBeDisabled()
    expect(invokeMock).not.toHaveBeenCalledWith('conflict_choose_cloud', expect.anything())
    await user.click(screen.getByText('云端的事项'))
    await user.click(screen.getByText('使用云端'))
    expect(invokeMock).toHaveBeenCalledWith('conflict_choose_cloud', { arg: { candidateId: next.candidateId, cloudVersion: '9007199254740993' } })
  })

  it('稍后决定走 defer（不提交覆盖）', async () => {
    installInvoke({ init_state: () => conflictStartup(), conflict_defer: { message: '', offersUndo: false, id: null } })
    const user = userEvent.setup()
    render(
      <DoingProvider>
        <ConflictOverlay />
        <SyncBadge />
      </DoingProvider>,
    )
    await user.click(await screen.findByText('稍后决定'))
    expect(invokeMock).toHaveBeenCalledWith('conflict_defer')
    expect(invokeMock).not.toHaveBeenCalledWith('conflict_choose_local', expect.anything())
    expect(invokeMock).not.toHaveBeenCalledWith('conflict_choose_cloud', expect.anything())
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
    await user.click(screen.getByTitle('查看同步详情'))
    await user.click(screen.getByText('处理冲突'))
    expect(screen.getByRole('dialog')).toBeInTheDocument()

  })
})

describe('登录表单（校验与错误恢复）', () => {
  function loggedOutStartup(): StartupView {
    const s = startup()
    return { ...s, auth: { ...s.auth, loggedIn: false } }
  }

  it('空表单禁用提交；服务端拒绝时显示错误', async () => {
    installInvoke({ init_state: () => loggedOutStartup(), auth_login: () => Promise.reject('用户名或密码不正确') })
    const user = userEvent.setup()
    render(
      <DoingProvider>
        <LoginView />
      </DoingProvider>,
    )
    const submit = await screen.findByText('登录')
    expect(submit.closest('button')).toBeDisabled()
    await user.type(screen.getByPlaceholderText('你的用户名'), 'user')
    await user.type(screen.getByPlaceholderText('输入密码'), 'secret')
    expect(submit.closest('button')).not.toBeDisabled()
    await user.click(submit)
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('auth_login', { arg: { username: 'user', password: 'secret' } })
    })
    expect(await screen.findByRole('alert')).toHaveTextContent('用户名或密码不正确')
  })
})

describe('设置页（回滚导出，计划 §10.1 迁移回滚面）', () => {
  async function openDataSection() {
    const user = userEvent.setup()
    render(
      <DoingProvider>
        <SettingsApp />
      </DoingProvider>,
    )
    await user.click(await screen.findByText('数据与关于'))
    return user
  }

  it('导出兼容旧版成功时展示导出文件路径', async () => {
    const exportPath = '/tmp/doing/exports/doing-legacy-export-20260910-024000.json'
    installInvoke({ data_export_legacy: exportPath })
    const user = await openDataSection()
    await user.click(await screen.findByText('导出兼容旧版（回滚用）…'))
    expect(invokeMock).toHaveBeenCalledWith('data_export_legacy')
    const notice = await screen.findByRole('status')
    expect(notice).toHaveTextContent('已导出兼容旧版的数据：')
    expect(notice).toHaveTextContent(exportPath)
  })

  it('导出失败时以 alert 展示错误', async () => {
    installInvoke({ data_export_legacy: () => Promise.reject('导出失败：磁盘只读') })
    const user = await openDataSection()
    await user.click(await screen.findByText('导出兼容旧版（回滚用）…'))
    expect(await screen.findByRole('alert')).toHaveTextContent('导出失败：磁盘只读')
  })
})

// —— 以下用例映射旧版 37 项原生交互检查中的可自动化子集 ——

describe('行选择与键盘导航（点击选中 / ↑↓ / 空格 / 回车 / 退格）', () => {
  async function renderNav() {
    installInvoke({ init_state: () => navStartup() })
    const user = userEvent.setup()
    render(
      <DoingProvider>
        <Workspace panelMode={false} />
      </DoingProvider>,
    )
    await screen.findByText('买牛奶')
    return user
  }

  it('点击任务文本选中；空格完成该任务', async () => {
    const user = await renderNav()
    await user.click(screen.getByText('买牛奶'))
    await user.keyboard(' ')
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('task_toggle_done', { id: SECOND_ID })
    })
  })

  it('录入框按下箭头选中首行，再按空格完成（导航不受焦点限制）', async () => {
    const user = await renderNav()
    const composer = screen.getByLabelText('新事项')
    expect(document.activeElement).toBe(composer)
    await user.keyboard('{ArrowDown}')
    await user.keyboard(' ')
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('task_toggle_done', { id: SECOND_ID })
    })
  })

  it('录入框内输入空格不会完成任务（保留输入语义）', async () => {
    const user = await renderNav()
    await user.type(screen.getByLabelText('新事项'), '带 空格 的草稿')
    expect(invokeMock).not.toHaveBeenCalledWith('task_toggle_done', expect.anything())
    expect((screen.getByLabelText('新事项') as HTMLInputElement).value).toBe('带 空格 的草稿')
  })

  it('回车在选中行进入编辑；⌘↩ 提交修剪后的文本', async () => {
    const user = await renderNav()
    await user.click(screen.getByText('买牛奶'))
    await user.keyboard('{Enter}')
    const area = await screen.findByDisplayValue('买牛奶')
    await user.clear(area)
    await user.type(area, '  买牛奶改  ')
    await user.keyboard('{Meta>}{Enter}{/Meta}')
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('task_edit', { id: SECOND_ID, arg: { text: '买牛奶改' } })
    })
  })

  it('编辑器内 Escape 取消，不改数据；输入空格不完成任务', async () => {
    const user = await renderNav()
    await user.click(screen.getByText('买牛奶'))
    await user.keyboard('{Enter}')
    const area = await screen.findByDisplayValue('买牛奶')
    await user.clear(area)
    await user.type(area, 'a b')
    expect(invokeMock).not.toHaveBeenCalledWith('task_toggle_done', expect.anything())
    await user.keyboard('{Escape}')
    await waitFor(() => {
      expect(screen.queryByDisplayValue('a b')).toBeNull()
    })
    expect(invokeMock).not.toHaveBeenCalledWith('task_edit', expect.anything())
  })

  it('退格删除选中行', async () => {
    const user = await renderNav()
    await user.click(screen.getByText('买牛奶'))
    await user.keyboard('{Backspace}')
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('task_delete', { id: SECOND_ID })
    })
  })

  it('Escape 先清空录入草稿；空草稿再按才收起窗口', async () => {
    const user = await renderNav()
    const composer = screen.getByLabelText('新事项')
    await user.type(composer, '未完的草稿')
    await user.keyboard('{Escape}')
    expect((screen.getByLabelText('新事项') as HTMLInputElement).value).toBe('')
    expect(invokeMock).not.toHaveBeenCalledWith('system_hide_main')
    await user.keyboard('{Escape}')
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('system_hide_main')
    })
  })
})

describe('已完成折叠与行菜单', () => {
  async function renderNav() {
    installInvoke({ init_state: () => navStartup() })
    const user = userEvent.setup()
    render(
      <DoingProvider>
        <Workspace panelMode={false} />
      </DoingProvider>,
    )
    await screen.findByText('买牛奶')
    return user
  }

  it('已完成默认折叠，可展开/收起', async () => {
    const user = await renderNav()
    expect(screen.queryByText('已完成的事项')).toBeNull()
    await user.click(screen.getByText('已完成 · 1'))
    expect(await screen.findByText('已完成的事项')).toBeInTheDocument()
    await user.click(screen.getByText('已完成 · 1'))
    await waitFor(() => {
      expect(screen.queryByText('已完成的事项')).toBeNull()
    })
  })

  it('行菜单「设为当前焦点」调用 task_toggle_focus', async () => {
    const user = await renderNav()
    await user.click(screen.getByLabelText('事项操作：买牛奶'))
    await user.click(await screen.findByText('设为当前焦点'))
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('task_toggle_focus', { id: SECOND_ID })
    })
  })

  it('行菜单「删除事项」调用 task_delete', async () => {
    const user = await renderNav()
    await user.click(screen.getByLabelText('事项操作：买牛奶'))
    await user.click(await screen.findByText('删除事项'))
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('task_delete', { id: SECOND_ID })
    })
  })
})

describe('App 层全局快捷键（⌘N / ⌘Z）', () => {
  it('⌘N 聚焦录入框；⌘Z 触发撤销', async () => {
    installInvoke()
    render(<App />)
    const composer = await screen.findByLabelText('新事项')
    ;(document.activeElement as HTMLElement | null)?.blur()
    fireEvent.keyDown(document, { key: 'n', metaKey: true, bubbles: true })
    await waitFor(() => {
      expect(document.activeElement).toBe(composer)
    })
    fireEvent.keyDown(document, { key: 'z', metaKey: true, bubbles: true })
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('history_undo')
    })
  })

  it('编辑器内 ⌘Z 保留原生撤销（不触发 history_undo）', async () => {
    installInvoke({ init_state: () => navStartup() })
    const user = userEvent.setup()
    render(
      <DoingProvider>
        <Workspace panelMode={false} />
      </DoingProvider>,
    )
    await user.click(await screen.findByText('买牛奶'))
    await user.keyboard('{Enter}')
    await screen.findByDisplayValue('买牛奶')
    await user.keyboard('{Meta>}z{/Meta}')
    expect(invokeMock).not.toHaveBeenCalledWith('history_undo')
  })
})

describe('登录/注册（回车提交、等待期禁用、各自接口仅一次）', () => {
  function loggedOutStartup(): StartupView {
    const s = startup()
    return { ...s, auth: { ...s.auth, loggedIn: false } }
  }

  it('回车提交；等待期间按钮禁用且接口仅调用一次', async () => {
    let resolveLogin: (v: unknown) => void = () => {}
    installInvoke({
      init_state: () => loggedOutStartup(),
      auth_login: () => new Promise((r) => { resolveLogin = r }),
    })
    const user = userEvent.setup()
    render(
      <DoingProvider>
        <LoginView />
      </DoingProvider>,
    )
    await user.type(await screen.findByPlaceholderText('你的用户名'), 'user')
    await user.type(screen.getByPlaceholderText('输入密码'), 'secret')
    await user.keyboard('{Enter}')
    const busy = await screen.findByText('正在连接…')
    expect(busy.closest('button')).toBeDisabled()
    await user.keyboard('{Enter}')
    expect(invokeMock.mock.calls.filter(([c]) => c === 'auth_login')).toHaveLength(1)
    resolveLogin({ message: '', offersUndo: false, id: null })
    await waitFor(() => {
      expect(screen.getByText('登录')).toBeInTheDocument()
    })
  })

  it('创建账户切换走注册接口（密码至少 8 位），不调用登录接口', async () => {
    installInvoke({ init_state: () => loggedOutStartup() })
    const user = userEvent.setup()
    render(
      <DoingProvider>
        <LoginView />
      </DoingProvider>,
    )
    await user.click(await screen.findByText('创建账户'))
    await user.type(screen.getByPlaceholderText('你的用户名'), 'newbie')
    await user.type(screen.getByPlaceholderText('设置一个密码'), '12345678')
    await user.keyboard('{Enter}')
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('auth_register', { arg: { username: 'newbie', password: '12345678' } })
    })
    expect(invokeMock).not.toHaveBeenCalledWith('auth_login', expect.anything())
  })
})

describe('设置页外观选择', () => {
  it('点击浅色调用 settings_update 保存选择', async () => {
    installInvoke()
    const user = userEvent.setup()
    render(
      <DoingProvider>
        <SettingsApp />
      </DoingProvider>,
    )
    await user.click(await screen.findByText('浅色'))
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('settings_update', { patch: { appearance: 'light' } })
    })
  })
})

describe('应用事件与系统集成（迁移横幅 / 打开设置 / 会话失效 / 窗口快捷键）', () => {
  it('检测到旧数据时展示迁移横幅，点击导入调用 migration_import', async () => {
    const s = startup()
    installInvoke({
      init_state: () => ({
        ...s,
        legacyImportAvailable: true,
        migration: {
          eventRevision: 1,
          transactionId: null,
          recoveryRequired: false,
          preferencesAvailable: false,
          importedPreferences: false,
          requiresLogin: true,
          warnings: [],
          available: true,
          detectedFile: '/tmp/old/items.json',
          imported: false,
          error: null,
          backupPath: null,
        },
      }),
    })
    const user = userEvent.setup()
    render(<App />)
    expect(await screen.findByText('检测到旧版 Doing 数据')).toBeInTheDocument()
    await user.click(screen.getByText('导入'))
    expect(invokeMock).not.toHaveBeenCalledWith('migration_import', expect.anything())
    await user.click(screen.getByRole('button', { name: '确认导入' }))
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('migration_import', { source: '/tmp/old/items.json', preferences: false })
    })
  })

  it('主面板未显示时，设置窗口也保留待恢复迁移入口', async () => {
    windowState.label = 'settings'
    const initial = startup()
    const id = '11111111-1111-4111-8111-111111111111'
    installInvoke({ init_state: () => ({ ...initial, migration: {
      eventRevision: 1, transactionId: id, available: false, detectedFile: '/fixture/items.json',
      recoveryRequired: true, preferencesAvailable: false, importedPreferences: false, requiresLogin: true,
      warnings: [], imported: false, error: '待核对来源', backupPath: '/fixture/backup.json',
    } }) })
    const user = userEvent.setup()
    render(<App />)
    expect(await screen.findByText('旧版导入尚未完成')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: '继续恢复' }))
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('migration_resume', { transactionId: id }))
    expect(invokeMock).not.toHaveBeenCalledWith('system_open_settings')
  })

  it('doing://open-settings 事件：写入一次性板块请求并打开设置窗口', async () => {
    installInvoke()
    render(<App />)
    await screen.findByLabelText('新事项')
    await act(async () => {
      eventBus.emit('doing://open-settings', 'account')
    })
    expect(localStorage.getItem('doing.openSettingsSection')).toBe('account')
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('system_open_settings')
    })
  })

  it('doing://session-lost 事件：回到登录视图', async () => {
    installInvoke()
    render(<App />)
    await screen.findByLabelText('新事项')
    await act(async () => {
      eventBus.emit('doing://session-lost', { ...startupFixture(2, 10).auth, loggedIn: false, username: null, error: '登录状态已失效，请重新登录' })
    })
    expect(await screen.findByPlaceholderText('你的用户名')).toBeInTheDocument()
  })

  it('⌘W 隐藏主面板；⌘, 打开设置窗口', async () => {
    installInvoke()
    render(<App />)
    await screen.findByLabelText('新事项')
    fireEvent.keyDown(document, { key: 'w', metaKey: true })
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('system_hide_main')
    })
    fireEvent.keyDown(document, { key: ',', metaKey: true })
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('system_open_settings')
    })
  })

  it('钉在桌面（模式切换）触发 system_toggle_mode', async () => {
    installInvoke()
    const user = userEvent.setup()
    render(
      <DoingProvider>
        <Workspace panelMode={false} />
      </DoingProvider>,
    )
    await user.click(await screen.findByLabelText('钉在桌面上'))
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('system_toggle_mode')
    })
  })
})

describe('设置页板块定位（托盘「从云端恢复…」一次性请求）', () => {
  it('读取 localStorage 板块请求并定位到账户，随后清除', async () => {
    localStorage.setItem('doing.openSettingsSection', 'account')
    installInvoke()
    render(
      <DoingProvider>
        <SettingsApp />
      </DoingProvider>,
    )
    expect(await screen.findByText('Doing 账户')).toBeInTheDocument()
    expect(localStorage.getItem('doing.openSettingsSection')).toBeNull()
  })
})

describe('IME 组合输入守卫', () => {
  it('录入框：组合期间回车不提交，组合结束后回车提交', async () => {
    installInvoke()
    const user = userEvent.setup()
    render(
      <DoingProvider>
        <Workspace panelMode={false} />
      </DoingProvider>,
    )
    const input = await screen.findByLabelText('新事项')
    await user.type(input, '中文候选')
    fireEvent.keyDown(input, { key: 'Enter', isComposing: true })
    expect(invokeMock).not.toHaveBeenCalledWith('task_add', expect.anything())
    fireEvent.keyDown(input, { key: 'Enter' })
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('task_add', { arg: { text: '中文候选' }, due: null })
    })
  })

  it('行内编辑器：组合期间回车不提交，组合结束后回车提交', async () => {
    installInvoke({ init_state: () => navStartup() })
    const user = userEvent.setup()
    render(
      <DoingProvider>
        <Workspace panelMode={false} />
      </DoingProvider>,
    )
    await user.click(await screen.findByText('买牛奶'))
    await user.keyboard('{Enter}')
    const area = await screen.findByDisplayValue('买牛奶')
    fireEvent.change(area, { target: { value: '组合后文本' } })
    fireEvent.compositionStart(area)
    fireEvent.keyDown(area, { key: 'Enter' })
    expect(invokeMock).not.toHaveBeenCalledWith('task_edit', expect.anything())
    fireEvent.compositionEnd(area)
    fireEvent.keyDown(area, { key: 'Enter' })
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('task_edit', { id: SECOND_ID, arg: { text: '组合后文本' } })
    })
  })
})


describe('账号会话与表单隔离', () => {
  it('即使登出事件丢失，直接收到 B 登录也不会保留 A 的录入草稿', async () => {
    installInvoke()
    const user = userEvent.setup()
    render(<App />)
    await user.type(await screen.findByLabelText('新事项'), '不能跨账号提交的草稿')
    act(() => {
      eventBus.emit('doing://auth-state', { ...startupFixture(4, 20).auth, username: 'account-b' })
      eventBus.emit('doing://snapshot', startupFixture(4, 21).snapshot)
    })
    expect(await screen.findByLabelText('新事项')).toHaveValue('')
    act(() => eventBus.emit('doing://session-lost', { ...startupFixture(2, 19).auth, loggedIn: false }))
    expect(screen.getByLabelText('新事项')).toBeInTheDocument()
  })

  it('系统安全存储/重启恢复的认证错误由登录界面展示', async () => {
    const state = startup()
    state.auth = { ...state.auth, loggedIn: false, error: '系统安全存储不可用，请检查权限' }
    installInvoke({ init_state: state })
    render(<App />)
    expect(await screen.findByRole('alert')).toHaveTextContent('系统安全存储不可用')
  })
})


describe('原生焦点事件边界（jsdom 协议测试，不替代真实输入法验收）', () => {
  it('固定浮窗失焦时不自动隐藏', async () => {
    const state = startup(); state.settings.mode = 'panel'
    installInvoke({ init_state: state })
    render(<App />)
    await screen.findByLabelText('新事项')
    Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true })
    vi.useFakeTimers()
    act(() => eventBus.emit('test://native-focus', false))
    await act(async () => vi.advanceTimersByTimeAsync(200))
    expect(invokeMock).not.toHaveBeenCalledWith('system_hide_main')
  })
  it('composition 期间候选窗口夺焦不隐藏，结束后普通失焦才隐藏', async () => {
    installInvoke(); render(<App />)
    const input = await screen.findByLabelText('新事项')
    Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true })
    vi.useFakeTimers()
    fireEvent.compositionStart(input)
    act(() => eventBus.emit('test://native-focus', false))
    await act(async () => vi.advanceTimersByTimeAsync(200))
    expect(invokeMock).not.toHaveBeenCalledWith('system_hide_main')
    fireEvent.compositionEnd(input)
    act(() => eventBus.emit('test://native-focus', false))
    await act(async () => vi.advanceTimersByTimeAsync(200))
    expect(invokeMock).toHaveBeenCalledWith('system_hide_main')
  })
  it('延迟失焦检查前已重新获得焦点时不收起窗口', async () => {
    installInvoke(); render(<App />)
    await screen.findByLabelText('新事项')
    Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true })
    vi.useFakeTimers()
    act(() => eventBus.emit('test://native-focus', false))
    await act(async () => vi.advanceTimersByTimeAsync(50))
    act(() => eventBus.emit('test://native-focus', true))
    await act(async () => vi.advanceTimersByTimeAsync(200))
    expect(invokeMock).not.toHaveBeenCalledWith('system_hide_main')
  })
})

describe('持久通知定位与工作区 ACK', () => {
  for (const [name, itemId] of [['焦点卡', ITEM_ID], ['折叠的已完成任务', DONE_ID], ['普通待办', SECOND_ID]]) {
    it(`${name} 挂载并滚动后才确认通知消费`, async () => {
      let pending = true
      const id = '99999999-9999-4999-8999-999999999999'
      const scroll = vi.spyOn(Element.prototype, 'scrollIntoView')
      installInvoke({
        init_state: () => navStartup(),
        notification_next: () => pending ? { notificationId: id, itemId, sessionGeneration: 1, eventRevision: 2 } : null,
        notification_ack: () => { expect(scroll).toHaveBeenCalled(); pending = false },
      })
      render(<App />)
      await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('notification_ack', { notificationId: id, sessionGeneration: 1 }))
      expect(invokeMock.mock.calls.filter(([command]) => command === 'notification_ack')).toHaveLength(1)
      if (itemId === DONE_ID) expect(screen.getByText('已完成的事项')).toBeVisible()
      scroll.mockRestore()
    })
  }
  it('同 UUID 的旧账号读取结果不得滚动或被确认', async () => {
    const id = '99999999-9999-4999-8999-999999999999'
    installInvoke({ notification_next: () => ({ notificationId: id, itemId: ITEM_ID, sessionGeneration: 0, eventRevision: 100 }) })
    render(<App />)
    await screen.findByLabelText('新事项')
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('notification_next'))
    expect(invokeMock).not.toHaveBeenCalledWith('notification_ack', expect.anything())
  })
})

describe('真实通知权限状态的展示', () => {
  it('未请求时明确申请权限，失败不伪显示为已允许', async () => {
    installInvoke({ notification_permission: () => ({ status: 'notDetermined', error: null }), notification_request_permission: () => ({ status: 'denied', error: null }) })
    const user = userEvent.setup()
    render(<DoingProvider><SettingsApp /></DoingProvider>)
    await user.click(await screen.findByText('提醒', { exact: true }))
    expect(await screen.findByText('尚未请求')).toBeInTheDocument()
    expect(invokeMock).not.toHaveBeenCalledWith('notification_request_permission')
    await user.click(screen.getByRole('button', { name: '申请通知权限' }))
    expect(await screen.findByText('已拒绝')).toBeInTheDocument()
    expect(screen.getByText('请在系统通知设置中允许 Doing，之后点重新查询。')).toBeInTheDocument()
    expect(screen.queryByText('已允许')).not.toBeInTheDocument()
  })
  it('原生查询不可用与已拒绝区分，不展示虚假的 Granted', async () => {
    installInvoke({ notification_permission: () => ({ status: 'unavailable', error: '需要应用安装身份' }) })
    const user = userEvent.setup()
    render(<DoingProvider><SettingsApp /></DoingProvider>)
    await user.click(await screen.findByText('提醒', { exact: true }))
    expect(await screen.findByText('暂不可用')).toBeInTheDocument()
    expect(screen.getByText('需要应用安装身份')).toBeInTheDocument()
    expect(screen.queryByText('已拒绝')).not.toBeInTheDocument()
    expect(screen.queryByText('已允许')).not.toBeInTheDocument()
  })
})
