// 前端类型入口：全部 DTO 由 Rust 生成（`src/types.gen.ts`，生成器与漂移测试见
// `src-tauri/src/bindings.rs`——计划 P1「禁止长期手写两份契约」）。
// 本文件只做再导出与展示层常量；契约由 Rust 漂移测试 + contract.test.ts 双重守卫。

import type { SyncStateName } from './types.gen'

export type {
  ItemView,
  SnapshotView,
  SyncStateName,
  SyncStatePayload,
  SettingsView,
  AuthStateView,
  MutationView,
  ConflictView,
  MigrationStatus,
  StartupView,
  TextArg,
  DueArg,
  MoveArg,
  AuthArg,
  CloudChoiceArg,
  SettingsPatch,
  BoolArg,
} from './types.gen'

/** 任务/UUID 在 JSON 里是字符串。 */
export type Uuid = string

export const SYNC_TEXT: Record<SyncStateName, string> = {
  idle: '空闲',
  pending: '等待同步',
  syncing: '同步中',
  synced: '已同步',
  failed: '同步失败',
  conflict: '需要处理冲突',
  unauthorized: '登录已失效',
}
