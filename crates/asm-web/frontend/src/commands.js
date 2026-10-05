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
// A pull that diverged is a different decision: a force push from the target would replace the hub's current copy with the target's divergent one.
const PULL_DIVERGED = 'The target machine has changes the sent copy does not. Decide which to keep there, then Retry.'
const DONE = { ok: 'The machine finished it.', in_sync: 'Already in sync, nothing to do.', already_applied: 'Already applied, nothing to do.', cancelled: 'Cancelled before it finished.' }

export const STATES = ['pending', 'queued', 'running', 'ok', 'blocked', 'cancelled', 'expired']
// `skipped` is not a hub state: it is a cancelled step whose `skipped` flag is set (an earlier step did not succeed).
export const STATE_LABEL = { pending: 'Waiting', queued: 'Queued', running: 'Running', ok: 'Done', blocked: 'Blocked', cancelled: 'Cancelled', expired: 'Expired', skipped: 'Skipped' }
export const STATE_TONE = { pending: 'neutral', queued: 'neutral', running: 'info', ok: 'success', blocked: 'warning', cancelled: 'neutral', expired: 'warning', skipped: 'neutral' }

/// The key a single command or step is badged with.
export const stateKey = (c) => (c.skipped ? 'skipped' : c.state)
/// Not final: queued, running, or (a plan step) waiting for its parent.
export const isOpen = (c) => c.state === 'pending' || c.state === 'queued' || c.state === 'running'
export const canRetry = (c) => !c.skipped && (c.state === 'blocked' || c.state === 'expired' || c.state === 'cancelled')
/// A step skipped because the plan was cancelled while its parent was still running: the parent then finished ok, so the plan can go on from here.
const resumable = (c, byId) => c.skipped && byId[c.needs]?.state === 'ok'

/// How long ago a machine last asked the hub for work, and whether that is long enough to warn about (a daemon polls every few seconds).
export const STALE_MS = 2 * 60_000
export function asked(machine, now = Date.now()) {
  const remote = machine?.remote
  if (!remote) return null
  const t = remote.polled_at ? Date.parse(remote.polled_at) : NaN
  if (Number.isNaN(t)) return { text: 'has not asked yet', stale: true }
  return { text: `last asked ${ago(remote.polled_at, now)}`, stale: now - t > STALE_MS }
}

/// The sentence under a command's state: the fixed hint for a code, else the hub's detail.
export function resultText(c, byId = {}, machine = null, now = Date.now()) {
  if (c.state === 'pending') {
    const n = byId[c.needs]?.step
    return n ? `Waiting for step ${n}.` : 'Waiting for the earlier step.'
  }
  if (c.skipped) return c.detail || 'Skipped: an earlier step did not succeed.'
  if (c.state === 'queued') {
    if (c.cancel_requested) return 'Cancel requested.'
    const a = asked(machine, now)
    return `Waiting for the machine to pick it up${a ? ` (${a.text})` : ''}.`
  }
  if (c.state === 'running') return c.cancel_requested ? 'Cancel requested. It may still finish.' : 'The machine is working on it.'
  const code = c.code || ''
  if (code === 'failed') return c.detail || 'It failed.'
  return (code === 'diverged' && c.op === 'pull' ? PULL_DIVERGED : HINTS[code]) || DONE[code] || c.detail || DONE[c.state] || ''
}

/// The hub's own sentence, when the fixed hint above does not already say it.
export const moreDetail = (c, byId = {}) => (isOpen(c) || c.skipped || !c.detail || c.detail === resultText(c, byId) ? '' : c.detail)

/// The plan state the hub derives from the steps: any step still going wins
/// (running, then queued, then waiting), then blocked, expired, cancelled; ok only when every step is.
export function planState(steps) {
  for (const s of ['running', 'queued', 'pending', 'blocked', 'cancelled', 'expired'])
    if (steps.some((c) => c.state === s && !(s === 'cancelled' && c.skipped))) return s
  return steps.every((c) => c.state === 'ok') ? 'ok' : 'cancelled'
}

/// One plan out of its steps, in step order. Its summary line: where it is, or where it stopped.
export function makePlan(id, list) {
  const steps = [...list].sort((a, b) => (a.step || 0) - (b.step || 0))
  const first = steps[0]
  const last = steps[steps.length - 1]
  const state = planState(steps)
  const at = steps.find((c) => c.state === state && !c.skipped) || (state === 'cancelled' ? steps.find((c) => c.skipped) : undefined)
  const progress = state === 'ok' ? `All ${steps.length} steps done` : at ? `${isOpen(at) ? 'Step' : 'Stopped at step'} ${at.step || steps.indexOf(at) + 1} of ${steps.length}` : ''
  return {
    plan: true, id, steps, state, progress, first, last,
    title: first.title, agent: first.agent, session: first.session,
    from: first.machine, to: last.machine, exact: !!first.args?.exact,
    created: steps.reduce((m, c) => (c.created < m ? c.created : m), first.created),
    cancel_requested: steps.some((c) => isOpen(c) && c.cancel_requested),
    canCancel: steps.some((c) => isOpen(c) && !c.cancel_requested),
    canRetry: !!retryStep(steps),
  }
}

/// The step Retry queues again: the first that did not succeed, or the one a cancel skipped after its parent had finished.
export function retryStep(steps) {
  const byId = Object.fromEntries(steps.map((c) => [c.id, c]))
  return steps.find(canRetry) || steps.find((c) => resumable(c, byId))
}

const words = (list) => (list.length < 2 ? list.join('') : `${list.slice(0, -1).join(', ')} and ${list[list.length - 1]}`)
/// What cancelling a plan will and will not do, from where its steps are now.
export function cancelText(plan) {
  const open = plan.steps.filter(isOpen)
  const out = []
  for (const c of plan.steps.filter((c) => c.state === 'ok')) out.push(`Step ${c.step} already finished${c.op === 'push' ? '; its copy stays on the hub' : ''}.`)
  for (const c of open.filter((c) => c.state === 'running')) out.push(`Step ${c.step} is running on ${c.machine.name} and may still finish.`)
  const idle = open.filter((c) => c.state !== 'running').map((c) => c.step)
  if (idle.length) out.push(`${idle.length > 1 ? 'Steps' : 'Step'} ${words(idle)} will not run.`)
  out.push('The source machine keeps its copy; nothing is archived or deleted.')
  return out.join(' ')
}

/// The step named the way a toast does: `step 2 (pull to B)`.
export const stepLabel = (c) => `step ${c.step} (${c.op} ${c.op === 'push' ? 'from' : 'to'} ${c.machine.name})`

/// The list the Commands tab shows: a plan's steps collapse into one entry (at the place of its first step), single commands stay as they are.
export function groupItems(commands) {
  const plans = new Map()
  for (const c of commands) if (c.plan) plans.set(c.plan, [...(plans.get(c.plan) || []), c])
  const seen = new Set()
  const out = []
  for (const c of commands) {
    if (!c.plan) out.push(c)
    else if (!seen.has(c.plan)) {
      seen.add(c.plan)
      out.push(makePlan(c.plan, plans.get(c.plan)))
    }
  }
  return out
}

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
export const whyNotTarget = (m, op) => (!m.remote ? 'has not reported remote control' : !m.remote.enabled ? 'remote control is off' : op && Array.isArray(m.remote.ops) && !m.remote.ops.includes(op) ? `does not allow ${op}` : '')
