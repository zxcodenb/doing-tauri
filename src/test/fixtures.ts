import type { ConflictView, StartupView } from '../types'

export const ITEM_ID = '11111111-1111-1111-1111-111111111111'
export const CANDIDATE_ID = 'aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa'

export function startupFixture(generation = 1, eventRevision = 1): StartupView {
  const stamp = { sessionGeneration: generation, eventRevision }
  return {
    auth: { ...stamp, loggedIn: true, username: 'tester', serverUrl: 'https://api.example.test', isAuthenticating: false, error: null },
    snapshot: {
      ...stamp, revision: 1,
      items: [{ id: ITEM_ID, text: '此账号的事项', done: false, createdAt: '2026-09-10T00:00:00Z', dueDate: null, updatedAt: '2026-09-10T00:00:00Z' }],
      focusId: ITEM_ID, undoTitle: null, redoTitle: null, notifiedDueIds: [], saveFailed: false,
    },
    settings: {
      revision: 1, eventRevision, error: null,
      appearance: 'system', mode: 'popover', showFocusInMenuBar: true, menuBarTextLimit: 18,
      notificationsEnabled: true, notificationSound: true, dueSoonEnabled: true, dueSoonHours: 24,
      showOverdueInMenuBar: true, showOverdueBanner: true, automaticSync: true,
    },
    sync: { ...stamp, state: 'idle', lastSyncAt: null, lastError: null, conflictCloudCount: null, conflictCloudVersion: null, conflictId: null },
    conflict: null, legacyImportAvailable: false, migration: null,
  }
}

export function conflictFixture(generation = 1, eventRevision = 2): ConflictView {
  return {
    sessionGeneration: generation, eventRevision, candidateId: CANDIDATE_ID,
    reason: '本地与云端快照不同', cloudVersion: '9007199254740993', cloudCount: 0, cloudPreview: [], updatedAt: null,
  }
}
