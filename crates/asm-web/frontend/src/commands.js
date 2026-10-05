import { ago } from './format.js'

// What the remote-control UI says about a command. Kept as plain data so the
// wording is testable and the hub's code names live in one place.

// Shown next to a blocked result. `failed` has no fixed sentence: the detail is the message.
export const HINTS = {
  diverged: 'Both machines changed this session. Resolve it on the machine, or push with force there.',
  ahead: "This machine's copy is newer than the one being pulled.",
  live: 'The session is running on that machine. Close it and retry.',
  no_dir: 'The project folder does not exist on that machine. Pull it there once with --project-dir.',
  not_restorable: "This agent's sessions cannot be restored onto a machine yet.",
  hub_newer: 'The hub has a newer copy from another machine. Pull it first.',
  conflict: 'Another machine pushed at the same time. Retry.',
  remote_off: 'Remote control is off on that machine (asm control enable).',
  unsupported: "That machine's asm is too old for this command.",
  expired: 'The machine did not pick this up in time. Retry when it is online.',
}
const DONE = { ok: 'The machine finished it.', in_sync: 'Already in sync, nothing to do.', already_applied: 'Already applied, nothing to do.', cancelled: 'Cancelled before it finished.' }

export const STATES = ['queued', 'running', 'ok', 'blocked', 'cancelled', 'expired']
export const STATE_LABEL = { queued: 'Queued', running: 'Running', ok: 'Done', blocked: 'Blocked', cancelled: 'Cancelled', expired: 'Expired' }
export const STATE_TONE = { queued: 'neutral', running: 'info', ok: 'success', blocked: 'warning', cancelled: 'neutral', expired: 'warning' }

export const isOpen = (c) => c.state === 'queued' || c.state === 'running'
export const canRetry = (c) => c.state === 'blocked' || c.state === 'expired' || c.state === 'cancelled'

/// The sentence under a command's state: the fixed hint for a code, else the hub's detail.
export function resultText(c) {
  if (c.state === 'queued') return c.cancel_requested ? 'Cancel requested.' : 'Waiting for the machine to pick it up.'
  if (c.state === 'running') return c.cancel_requested ? 'Cancel requested. It may still finish.' : 'The machine is working on it.'
  const code = c.code || ''
  if (code === 'failed') return c.detail || 'It failed.'
  return HINTS[code] || DONE[code] || c.detail || DONE[c.state] || ''
}

/// The hub's own sentence, when the fixed hint above does not already say it.
export const moreDetail = (c) => (isOpen(c) || !c.detail || c.detail === resultText(c) ? '' : c.detail)

/// The machines tab chip. `polled_at` within a minute counts as listening now.
export function remoteStatus(remote, now = Date.now()) {
  if (!remote) return { tone: 'neutral', label: 'Not reported', kind: 'unknown' }
  if (!remote.enabled) return { tone: 'neutral', label: 'Off', kind: 'off' }
  const t = remote.polled_at ? Date.parse(remote.polled_at) : NaN
  if (Number.isNaN(t)) return { tone: 'warning', label: 'On, never polled', kind: 'stale' }
  if (now - t < 60_000) return { tone: 'success', label: 'On', kind: 'on' }
  return { tone: 'warning', label: `On, last polled ${ago(remote.polled_at, now)}`, kind: 'stale' }
}

/// Why a machine cannot be chosen as a target, or '' when it can.
export const whyNotTarget = (m) => (!m.remote ? 'has not reported remote control' : !m.remote.enabled ? 'remote control is off' : '')
