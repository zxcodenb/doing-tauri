import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from '../lib/ipc'
import { useDoing } from './useDoing'
import type { ScrollTargetView } from '../types'

/** Rust owns a persistent queue. Window-shown is only a wakeup, never the sole copy of a click. */
export function useNotificationNavigation() {
  const { ready, auth, snapshot, windowShowRevision, run, pushNotice } = useDoing()
  const loggedIn = auth?.loggedIn ?? false
  const generation = auth?.sessionGeneration
  const [target, setTarget] = useState<ScrollTargetView | null>(null)
  const lifecycle = useRef({ mounted: false, sequence: 0, actor: undefined as number | undefined })
  const acknowledgments = useRef(new Set<string>())
  const lastError = useRef('')
  useEffect(() => {
    const state = lifecycle.current
    state.mounted = true
    return () => { state.mounted = false; state.sequence++ }
  }, [])
  useEffect(() => {
    lifecycle.current.actor = ready && loggedIn ? generation : undefined
    lifecycle.current.sequence++
  }, [ready, loggedIn, generation])
  const read = useCallback(() => {
    const state = lifecycle.current
    const request = ++state.sequence
    if (!ready || !loggedIn) return
    void api.notificationNext().then((value) => {
      if (!state.mounted || state.sequence !== request) return
      lastError.current = ''
      setTarget(value?.sessionGeneration === generation ? value : null)
    }).catch((error: unknown) => {
      if (!state.mounted || state.sequence !== request) return
      setTarget(null)
      const message = error instanceof Error ? error.message : '无法读取待定位通知'
      if (lastError.current !== message) { lastError.current = message; pushNotice({ kind: 'error', message }) }
    })
  }, [ready, loggedIn, generation, pushNotice])
  // Provider awaits all listener registrations before ready=true. Re-read on startup/login,
  // actual window presentation, or authoritative data changes, not just a transient click.
  useEffect(() => { read() }, [read, windowShowRevision, snapshot.eventRevision])

  const acknowledge = useCallback(async (value: ScrollTargetView) => {
    const state = lifecycle.current
    const key = `${value.sessionGeneration}:${value.notificationId}`
    if (!state.mounted || state.actor !== value.sessionGeneration || acknowledgments.current.has(key)) return
    acknowledgments.current.add(key)
    ++state.sequence // an old read may not reinstate an acknowledged activation
    const result = await run(() => api.notificationAck(value.notificationId, value.sessionGeneration))
    acknowledgments.current.delete(key)
    if (!state.mounted || !result.ok || state.actor !== value.sessionGeneration) return
    ++state.sequence
    setTarget((previous) => previous?.notificationId === value.notificationId && previous.sessionGeneration === value.sessionGeneration ? null : previous)
    read()
  }, [read, run])
  const current = ready && target && loggedIn && generation === target.sessionGeneration
    && snapshot.sessionGeneration === target.sessionGeneration ? target : null
  return { target: current, acknowledge }
}
