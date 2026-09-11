// 契约守护（对齐计划 P1）：
// - DTO 载荷：Rust 生成 src/types.gen.ts（生成器/漂移测试见 src-tauri/src/bindings.rs），
//   前端 types.ts 只做再导出——本测试守卫“生成物被完整再导出”与关键字段形态；
// - 事件名与命令清单：手写两侧，本测试 + Rust build.rs 清单双向守卫。
import { describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

function read(rel: string): string {
  return readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8')
}

const eventsRs = read('../../src-tauri/src/events.rs')
const libRs = read('../../src-tauri/src/lib.rs')
const buildRs = read('../../src-tauri/build.rs')
const capabilities = read('../../src-tauri/capabilities/default.json') + read('../../src-tauri/capabilities/notifications.json')
const typesTs = read('../types.ts')
const typesGenTs = read('../types.gen.ts')
const ipcTs = read('../lib/ipc.ts')

function buildCommands(): string[] {
  const block = buildRs.slice(buildRs.indexOf('const COMMANDS'), buildRs.indexOf('];'))
  return [...block.matchAll(/"([a-z_]+)"/g)].map((m) => m[1])
}

function handlerCommands(): string[] {
  const block = libRs.slice(libRs.indexOf('generate_handler!['))
  return [...block.matchAll(/commands::([a-z_]+)/g)].map((m) => m[1])
}

function capabilityAllows(): string[] {
  return [...capabilities.matchAll(/"allow-([a-z-]+)"/g)].map((m) => m[1].replace(/-/g, '_'))
}

describe('Rust ↔ TypeScript 契约', () => {
  it('事件名一一对应（doing:// 前缀）', () => {
    const rustEvents = [...eventsRs.matchAll(/pub const EVT_\w+: &str = "([^"]+)"/g)].map(
      (m) => m[1],
    )
    expect(rustEvents.length).toBeGreaterThan(8)
    for (const name of rustEvents) {
      expect(ipcTs, `ipc.ts 缺少事件 ${name}`).toContain(`'${name}'`)
    }
  })

  it('生成物（types.gen.ts）被 types.ts 完整再导出', () => {
    const generated = [...typesGenTs.matchAll(/export type (\w+)/g)].map((m) => m[1])
    expect(generated.length).toBeGreaterThan(15)
    for (const name of generated) {
      // 再导出块或本地 import；缺失说明生成物更新后忘了同步入口。
      expect(typesTs, `types.ts 未再导出生成类型 ${name}`).toContain(name)
    }
  })

  it('生成物字段为 camelCase 且与 Rust serde 对齐（抽样）', () => {
    const probes = [
      'focusId',
      'notifiedDueIds',
      'showFocusInMenuBar',
      'automaticSync',
      'cloudVersion',
      'legacyImportAvailable',
      'isAuthenticating',
    ]
    for (const p of probes) {
      expect(typesGenTs, `types.gen.ts 缺少字段 ${p}`).toContain(p)
    }
    // 反向：生成物不应包含 Rust 侧 snake_case 字段名。
    for (const p of ['focus_id', 'notified_due_ids', 'show_focus_in_menu_bar']) {
      expect(typesGenTs, `types.gen.ts 不应含 snake_case 字段 ${p}`).not.toContain(p)
    }
  })

  it('同步状态枚举：生成物字面量与展示文案表一致', () => {
    const states = ['idle', 'pending', 'syncing', 'synced', 'failed', 'conflict', 'unauthorized']
    for (const s of states) {
      expect(typesGenTs, `types.gen.ts 缺少状态字面量 ${s}`).toContain(`"${s}"`)
      expect(typesTs, `SYNC_TEXT 缺少 ${s}`).toContain(`${s}:`)
    }
    expect(typesTs).toContain('Record<SyncStateName, string>')
  })

  it('前端 invoke 的命令都已注册到 generate_handler', () => {
    const invoked = [...ipcTs.matchAll(/invoke<?[^(]*\('([a-z_]+)'/g)].map((m) => m[1])
    expect(invoked.length).toBeGreaterThan(20)
    const registered = handlerCommands()
    for (const cmd of invoked) {
      expect(registered, `lib.rs 未注册命令 ${cmd}`).toContain(cmd)
    }
    // 反向：Rust 命令都有前端入口。
    for (const cmd of registered) {
      expect(ipcTs, `ipc.ts 缺少命令封装 ${cmd}`).toContain(cmd)
    }
  })

  it('ACL：build.rs 命令清单 == generate_handler == capability allow 列表（双向）', () => {
    const manifest = buildCommands().sort()
    const handler = handlerCommands().sort()
    const allowed = capabilityAllows().sort()
    expect(manifest.length).toBeGreaterThan(25)
    // 命令清单与注册表一致（防漏登记）。
    expect(manifest).toEqual(handler)
    // capability 显式授权每条命令（未授权=运行期拒绝）。
    expect(allowed).toEqual(manifest)
  })
  it('通知定位读取和 ACK 只授权给主工作区，前端没有伪造系统点击入口', () => {
    const scoped = JSON.parse(read('../../src-tauri/capabilities/notifications.json'))
    expect(scoped.windows).toEqual(['main'])
    expect(scoped.permissions).toEqual(['allow-notification-next', 'allow-notification-ack'])
    const shared = JSON.parse(read('../../src-tauri/capabilities/default.json')).permissions
    expect(shared).not.toContain('allow-notification-next')
    expect(shared).not.toContain('allow-notification-ack')
    expect(ipcTs).not.toContain('system_notify_clicked')
    expect(capabilities).not.toContain('notification:')
  })
  it('Windows 安装方案只登记应用专用通知协议，运行时不抢占 URL 协议', () => {
    const config = JSON.parse(read('../../src-tauri/tauri.windows.conf.json'))
    const scheme = /ACTIVATION_SCHEME: &str = "([^"]+)"/.exec(read('../../src-tauri/src/reminder.rs'))?.[1]
    expect(config.plugins['deep-link'].desktop.schemes).toEqual([scheme])
    expect(config.bundle.targets).toEqual(['nsis'])
    expect(config.bundle.windows.nsis.installMode).toBe('currentUser')
    expect(libRs).not.toMatch(/\.register_all\(/)
    expect(libRs).toContain('parse_activation_uri')
  })
  it('设置窗口不得在启动配置中预载，按需创建并允许单独关闭', () => {
    const config = JSON.parse(read('../../src-tauri/tauri.conf.json'))
    expect(config.app.windows.map((window: { label: string }) => window.label)).toEqual(['main'])
    expect(read('../../src-tauri/src/desktop.rs')).toContain('WebviewWindowBuilder::new')
    const capability = JSON.parse(read('../../src-tauri/capabilities/settings.json'))
    expect(capability.windows).toEqual(['settings'])
    expect(capability.permissions).toEqual(['core:window:allow-close'])
  })

})
