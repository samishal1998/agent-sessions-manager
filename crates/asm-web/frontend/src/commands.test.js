import { expect, test } from 'bun:test'
import { HINTS, asked, canRetry, cancelText, groupItems, isOpen, makePlan, planState, remoteStatus, resultText, retryStep, stateKey, stepLabel, whyNotTarget } from './commands.js'

const now = Date.parse('2026-01-01T12:00:00Z')
const at = (s) => new Date(now - s * 1000).toISOString()

test('remote status chip', () => {
  expect(remoteStatus(null, now).label).toBe('Not reported')
  expect(remoteStatus({ enabled: false, polled_at: at(5) }, now).label).toBe('Off')
  expect(remoteStatus({ enabled: true, polled_at: at(59) }, now).label).toBe('On')
  expect(remoteStatus({ enabled: true, polled_at: at(2 * 3600) }, now).label).toBe('On, last polled 2h ago')
  expect(remoteStatus({ enabled: true, polled_at: null }, now).label).toBe('On, never polled')
})

test('result text', () => {
  expect(resultText({ state: 'blocked', code: 'live' })).toBe(HINTS.live)
  expect(resultText({ state: 'blocked', code: 'failed', detail: 'disk full' })).toBe('disk full')
  expect(resultText({ state: 'ok', code: 'in_sync' })).toMatch(/in sync/)
  expect(resultText({ state: 'expired', code: 'expired' })).toBe(HINTS.expired)
  expect(resultText({ state: 'running', cancel_requested: true })).toMatch(/may still finish/)
})

test('targets', () => {
  expect(whyNotTarget({ remote: null })).toBeTruthy()
  expect(whyNotTarget({ remote: { enabled: false } })).toBeTruthy()
  expect(whyNotTarget({ remote: { enabled: true } })).toBe('')
})

const step = (n, state, more = {}) => ({ id: `c${n}`, plan: 'p1', step: n, op: n === 1 ? 'push' : 'pull', agent: 'claude-code', session: 's', title: 'T', machine: { id: `m${n}`, name: `M${n}` }, state, created: at(100 - n), ...more })
const planOf = (a, b) => makePlan('p1', [step(1, ...[].concat(a)), step(2, ...[].concat(b))])

test('plan state follows the steps', () => {
  expect(planOf('ok', 'running').state).toBe('running')
  expect(planOf('queued', 'pending').state).toBe('queued')
  expect(planOf('ok', 'ok').state).toBe('ok')
  expect(planOf('blocked', ['cancelled', { skipped: true }]).state).toBe('blocked')
  expect(planOf('cancelled', ['cancelled', { skipped: true }]).state).toBe('cancelled')
  expect(planOf('expired', ['cancelled', { skipped: true }]).state).toBe('expired')
  expect(planOf('ok', 'expired').state).toBe('expired')
  expect(planOf('ok', 'blocked').state).toBe('blocked')
  expect(planOf('ok', 'cancelled').state).toBe('cancelled')
  expect(planOf('running', 'pending').state).toBe('running')
})

test('plan summary and actions', () => {
  const run = planOf('ok', 'running')
  expect(run.progress).toBe('Step 2 of 2')
  expect(run.canCancel).toBe(true)
  expect(run.canRetry).toBe(false)
  expect(planOf('ok', 'ok').progress).toBe('All 2 steps done')
  const stopped = planOf('blocked', ['cancelled', { skipped: true }])
  expect(stopped.progress).toBe('Stopped at step 1 of 2')
  expect(stopped.canRetry).toBe(true)
  expect(stopped.canCancel).toBe(false)
  expect(planOf('running', ['pending', { needs: 'c1' }]).canCancel).toBe(true)
  expect(planOf(['running', { cancel_requested: true }], 'pending').cancel_requested).toBe(true)
  expect(run.from.name).toBe('M1')
  expect(run.to.name).toBe('M2')
})

test('skipped and waiting steps', () => {
  const byId = { c1: step(1, 'queued') }
  expect(resultText(step(2, 'pending', { needs: 'c1' }), byId)).toBe('Waiting for step 1.')
  const skipped = step(2, 'cancelled', { skipped: true, code: 'cancelled', detail: 'step 1 did not succeed' })
  expect(stateKey(skipped)).toBe('skipped')
  expect(canRetry(skipped)).toBe(false)
  expect(resultText(skipped)).toBe('step 1 did not succeed')
  expect(isOpen(step(2, 'pending'))).toBe(true)
})

test('plan steps collapse into one entry, single commands stay', () => {
  const one = { id: 'x', op: 'push', state: 'ok', machine: { id: 'm', name: 'M' } }
  const items = groupItems([one, step(2, 'pending'), step(1, 'queued')])
  expect(items.length).toBe(2)
  expect(items[0]).toBe(one)
  expect(items[1].plan).toBe(true)
  expect(items[1].steps.map((c) => c.step)).toEqual([1, 2])
  expect(items[1].created).toBe(at(99))
})

test('a machine must allow the op', () => {
  const m = { remote: { enabled: true, ops: ['pull'] } }
  expect(whyNotTarget(m, 'push')).toMatch(/does not allow push/)
  expect(whyNotTarget(m, 'pull')).toBe('')
})

test('a plan cancelled while its push ran can be retried from the skipped step', () => {
  const cancelled = planOf('ok', ['cancelled', { skipped: true, needs: 'c1', code: 'cancelled', detail: 'cancelled with the plan before step 1 finished' }])
  expect(cancelled.state).toBe('cancelled')
  expect(cancelled.progress).toBe('Stopped at step 2 of 2')
  expect(cancelled.canRetry).toBe(true)
  expect(retryStep(cancelled.steps).step).toBe(2)
  expect(resultText(cancelled.steps[1])).toBe('cancelled with the plan before step 1 finished')
  // Skipped behind a step that did not succeed: the failed step is what Retry queues.
  const blocked = planOf('blocked', ['cancelled', { skipped: true, needs: 'c1' }])
  expect(retryStep(blocked.steps).step).toBe(1)
  // Skipped but its parent never finished ok and is not retryable itself: nothing to retry.
  expect(planOf('running', ['cancelled', { skipped: true, needs: 'c1' }]).canRetry).toBe(false)
})

test('cancel confirmation is built from the steps', () => {
  const saysCopy = 'The source machine keeps its copy; nothing is archived or deleted.'
  expect(cancelText(planOf('ok', 'queued'))).toBe(`Step 1 already finished; its copy stays on the hub. Step 2 will not run. ${saysCopy}`)
  expect(cancelText(planOf('running', 'pending'))).toBe(`Step 1 is running on M1 and may still finish. Step 2 will not run. ${saysCopy}`)
  expect(cancelText(planOf('queued', 'pending'))).toBe(`Steps 1 and 2 will not run. ${saysCopy}`)
})

test('a retried step is named', () => {
  expect(stepLabel(step(2, 'blocked'))).toBe('step 2 (pull to M2)')
  expect(stepLabel(step(1, 'blocked'))).toBe('step 1 (push from M1)')
})

test('a queued step says when its machine last asked', () => {
  const m = (s) => ({ remote: { enabled: true, polled_at: at(s) } })
  expect(resultText(step(2, 'queued'), {}, m(5), now)).toBe('Waiting for the machine to pick it up (last asked just now).')
  expect(resultText(step(2, 'queued'), {}, m(3 * 86400), now)).toBe('Waiting for the machine to pick it up (last asked 3d ago).')
  expect(resultText(step(2, 'queued'), {}, null, now)).toBe('Waiting for the machine to pick it up.')
  expect(asked(m(119), now).stale).toBe(false)
  expect(asked(m(121), now).stale).toBe(true)
  expect(asked({ remote: { enabled: true, polled_at: null } }, now)).toEqual({ text: 'has not asked yet', stale: true })
  expect(asked({ remote: null }, now)).toBe(null)
})

test('a diverged pull step does not suggest a force push', () => {
  expect(resultText({ state: 'blocked', code: 'diverged', op: 'push' })).toMatch(/push with force/)
  const pull = resultText({ state: 'blocked', code: 'diverged', op: 'pull' })
  expect(pull).toBe('The target machine has changes the sent copy does not. Decide which to keep there, then Retry.')
  expect(pull).not.toMatch(/force/)
})

test('a plan with a cancelled step and an expired one is cancelled, as the hub says', () => {
  expect(planState([{ state: 'expired' }, { state: 'cancelled' }])).toBe('cancelled')
})
