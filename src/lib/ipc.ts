// 统一 IPC 封装：命令调用 + 事件订阅（载荷类型由 Rust 生成，见 types.ts）。

import { invoke as nativeInvoke, type InvokeArgs } from '@tauri-apps/api/core'
import { CommandFailure } from './errors'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type {
  AuthArg,
  AuthStateView,
  BoolArg,
  CloudChoiceArg,
  ConflictView,
  DueArg,
  MigrationStatus,
  NotificationPermissionView,
  MoveArg,
  SettingsPatch,
  SettingsView,
  SnapshotView,
  ScrollTargetView,
  StartupView,
  SyncStatePayload,
  TextArg,
} from '../types'

export const EVT = {
  snapshot: 'doing://snapshot',
  syncState: 'doing://sync-state',
  authState: 'doing://auth-state',
  settings: 'doing://settings',
  sessionLost: 'doing://session-lost',
  conflict: 'doing://conflict',
  migration: 'doing://migration',
  saveFailed: 'doing://save-failed',
  openSettings: 'doing://open-settings',
  windowShown: 'doing://window-shown',
  windowBlurred: 'doing://window-blurred',
} as const

export type Unlisten = UnlistenFn

export async function on<T>(event: string, handler: (payload: T) => void): Promise<Unlisten> {
  return listen<T>(event, (e) => handler(e.payload))
}

async function invoke<T>(command: string, args?: InvokeArgs): Promise<T> {
  try { return args === undefined ? await nativeInvoke<T>(command) : await nativeInvoke<T>(command, args) }
  catch (error) { throw new CommandFailure(error) }
}

// MARK: 命令

export const api = {
  initState: () => invoke<StartupView>('init_state'),
  taskAdd: (text: string, due: string | null) =>
    invoke('task_add', { arg: { text } satisfies TextArg, due }),
  taskEdit: (id: string, text: string) =>
    invoke('task_edit', { id, arg: { text } satisfies TextArg }),
  taskSetDue: (id: string, due: string | null) =>
    invoke('task_set_due', { id, arg: { due } satisfies DueArg }),
  taskToggleDone: (id: string) => invoke('task_toggle_done', { id }),
  taskDelete: (id: string) => invoke('task_delete', { id }),
  taskToggleFocus: (id: string) => invoke('task_toggle_focus', { id }),
  taskClearCompleted: () => invoke('task_clear_completed'),
  taskMove: (id: string, targetId: string) =>
    invoke('task_move', { arg: { id, targetId } satisfies MoveArg }),
  historyUndo: () => invoke('history_undo'),
  historyRedo: () => invoke('history_redo'),
  authLogin: (username: string, password: string) =>
    invoke('auth_login', { arg: { username, password } satisfies AuthArg }),
  authRegister: (username: string, password: string) =>
    invoke('auth_register', { arg: { username, password } satisfies AuthArg }),
  authLogout: () => invoke('auth_logout'),
  syncFlush: () => invoke('sync_flush'),
  syncRestore: () => invoke('sync_restore'),
  conflictChooseLocal: (candidateId: string, cloudVersion: string) =>
    invoke('conflict_choose_local', { arg: { candidateId, cloudVersion } satisfies CloudChoiceArg }),
  conflictChooseCloud: (candidateId: string, cloudVersion: string) =>
    invoke('conflict_choose_cloud', { arg: { candidateId, cloudVersion } satisfies CloudChoiceArg }),
  conflictDefer: () => invoke('conflict_defer'),
  settingsUpdate: (patch: SettingsPatch) => invoke<SettingsView>('settings_update', { patch }),
  settingsReset: () => invoke<SettingsView>('settings_reset'),
  systemOpenSettings: () => invoke('system_open_settings'),
  systemHideMain: () => invoke('system_hide_main'),
  systemToggleMode: () => invoke<boolean>('system_toggle_mode'),
  systemQuit: () => invoke('system_quit'),
  systemSaveNow: () => invoke<boolean>('system_save_now'),
  systemDataPath: () => invoke<string>('system_data_path'),
  systemRevealData: () => invoke('system_reveal_data'),
  notificationPermission: () => invoke<NotificationPermissionView>('notification_permission'),
  notificationRequestPermission: () => invoke<NotificationPermissionView>('notification_request_permission'),
  notificationNext: () => invoke<ScrollTargetView | null>('notification_next'),
  notificationAck: (notificationId: string, sessionGeneration: number) => invoke<void>('notification_ack', { notificationId, sessionGeneration }),
  migrationImport: (source: string, preferences = false) => invoke<MigrationStatus>('migration_import', { source, preferences }),
  migrationResume: (transactionId: string) => invoke<MigrationStatus>('migration_resume', { transactionId }),
  migrationCancel: (transactionId: string) => invoke<MigrationStatus>('migration_cancel', { transactionId }),
  migrationKeepCurrent: (transactionId: string) => invoke<MigrationStatus>('migration_keep_current', { transactionId }),
  dataExportLegacy: () => invoke<string>('data_export_legacy'),
  launchAtLoginGet: () => invoke<boolean>('launch_at_login_get'),
  launchAtLoginSet: (enabled: boolean) =>
    invoke<string>('launch_at_login_set', { arg: { enabled } satisfies BoolArg }),
}

// 事件总线（无状态；由 hooks/store 消费）
export const bus = {
  snapshot: (fn: (p: SnapshotView) => void) => on<SnapshotView>(EVT.snapshot, fn),
  syncState: (fn: (p: SyncStatePayload) => void) => on<SyncStatePayload>(EVT.syncState, fn),
  authState: (fn: (p: AuthStateView) => void) => on<AuthStateView>(EVT.authState, fn),
  settings: (fn: (p: SettingsView) => void) => on<SettingsView>(EVT.settings, fn),
  sessionLost: (fn: (p: AuthStateView) => void) => on<AuthStateView>(EVT.sessionLost, fn),
  conflict: (fn: (p: ConflictView) => void) => on<ConflictView>(EVT.conflict, fn),
  migration: (fn: (p: MigrationStatus) => void) => on<MigrationStatus>(EVT.migration, fn),
  saveFailed: (fn: (message: string) => void) => on<string>(EVT.saveFailed, fn),
  openSettings: (fn: (section: string) => void) => on<string>(EVT.openSettings, fn),
  windowShown: (fn: () => void) => on(EVT.windowShown, fn),
}
