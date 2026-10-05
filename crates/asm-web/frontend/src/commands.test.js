import { expect, test } from 'bun:test'
import { ARCHIVE_V, HINTS, asked, canRetry, cancelText, groupItems, hintFor, isOpen, makePlan, planKind, planState, remoteStatus, resultText, retryStep, stateKey, stepLabel, whyNotSource, whyNotTarget } from './commands.js'

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

// A move: push on A, pull on B (needs 1), archive on A (needs 2).
const mstep = (n, state, more = {}) => ({
  id: `c${n}`, plan: 'p1', step: n, op: ['push', 'pull', 'archive'][n - 1], agent: 'claude-code', session: 's', title: 'T',
  machine: { id: n === 2 ? 'mB' : 'mA', name: n === 2 ? 'B' : 'A' }, state, created: at(100 - n), ...(n > 1 ? { needs: `c${n - 1}` } : {}), ...more,
})
const moveOf = (a, b, c) => makePlan('p1', [mstep(1, ...[].concat(a)), mstep(2, ...[].concat(b)), mstep(3, ...[].concat(c))])
const skip = ['cancelled', { skipped: true, code: 'cancelled', detail: 'step 2 did not succeed' }]

test('a plan is a move when it archives, a send otherwise', () => {
  const m = moveOf('ok', 'ok', 'queued')
  expect(m.kind).toBe('move')
  expect(planKind(planOf('ok', 'ok').steps)).toBe('send')
  expect(planOf('ok', 'ok').kind).toBe('send')
  // The hub's own word wins when it is on the steps.
  expect(planKind([{ plan_kind: 'move', op: 'push' }])).toBe('move')
  // The move runs from A and pulls to B: the archive on A is not "where it went".
  expect(m.from.name).toBe('A')
  expect(m.to.name).toBe('B')
})

test('plan state with three steps', () => {
  expect(moveOf('ok', 'ok', 'ok').state).toBe('ok')
  expect(moveOf('ok', 'ok', 'running').state).toBe('running')
  expect(moveOf('ok', 'ok', 'queued').state).toBe('queued')
  expect(moveOf('queued', 'pending', 'pending').state).toBe('queued')
  expect(moveOf('running', 'pending', 'pending').state).toBe('running')
  expect(moveOf('ok', 'ok', 'blocked').state).toBe('blocked')
  expect(moveOf('ok', 'blocked', skip).state).toBe('blocked')
  expect(moveOf('ok', 'expired', skip).state).toBe('expired')
  expect(moveOf('ok', 'ok', 'cancelled').state).toBe('cancelled')
  expect(moveOf('ok', 'cancelled', skip).state).toBe('cancelled')
})

test('move summary names the step it is at or stopped at', () => {
  expect(moveOf('ok', 'ok', 'ok').progress).toBe('All 3 steps done')
  expect(moveOf('ok', 'running', 'pending').progress).toBe('Step 2 of 3')
  expect(moveOf('ok', 'ok', 'queued').progress).toBe('Step 3 of 3')
  expect(moveOf('ok', 'ok', 'blocked').progress).toBe('Stopped at step 3 of 3')
  expect(moveOf('ok', 'blocked', skip).progress).toBe('Stopped at step 2 of 3')
})

test('retry on a move queues the step that did not succeed', () => {
  // An archive that was refused for another reason: retry it. (changed_since_move needs a new move: see below.)
  const late = moveOf('ok', 'ok', ['blocked', { code: 'live' }])
  expect(late.canRetry).toBe(true)
  expect(retryStep(late.steps).step).toBe(3)
  const mid = moveOf('ok', ['blocked', { code: 'no_dir' }], skip)
  expect(retryStep(mid.steps).step).toBe(2)
  expect(stepLabel(mstep(3, 'blocked'))).toBe('step 3 (archive on A)')
  expect(moveOf('ok', 'ok', 'ok').canRetry).toBe(false)
})

test('archive steps say what happened', () => {
  const ok = mstep(3, 'ok', { code: 'ok', detail: 'archived here; asm unarchive brings it back' })
  expect(resultText(ok)).toBe('Archived on A. asm unarchive brings it back.')
  expect(resultText(mstep(3, 'ok', { code: 'already_applied', detail: 'already archived' }))).toBe('Already applied, nothing to do.')
  expect(resultText(mstep(3, 'blocked', { code: 'changed_since_move' }))).toBe(HINTS.changed_since_move)
  expect(HINTS.changed_since_move).toBe('The session was continued on the source after the copy that was sent, so it was not archived. The destination has the sent copy. To move the current state, run the move again.')
  expect(resultText(mstep(3, 'blocked', { code: 'not_archivable' }))).toBe('This session cannot be archived on that machine.')
  expect(resultText(mstep(3, 'blocked', { code: 'live' }))).toBe('The session is running on the source. Close it without continuing it, then Retry. If you continue it, run the move again.')
  expect(resultText(mstep(3, 'pending', { needs: 'c2' }), { c2: mstep(2, 'queued') })).toBe('Not started: A keeps its session until step 2 succeeds; nothing has been archived.')
  expect(resultText(mstep(3, 'cancelled', { skipped: true, detail: 'step 2 did not succeed' }))).toBe('step 2 did not succeed')
})

test('cancelling a move says the archive will not run', () => {
  expect(cancelText(moveOf('ok', 'queued', 'pending'))).toBe('Step 1 already finished; its copy stays on the hub. Steps 2 and 3 will not run. The archive will not run unless it has already started: A keeps its session, and nothing is deleted.')
  expect(cancelText(moveOf('ok', 'running', 'pending'))).toMatch(/Step 2 is running on B and may still finish\. Step 3 will not run\. The archive will not run unless it has already started/)
  // Past the point of no return, the text does not claim nothing was archived.
  expect(cancelText(moveOf('ok', 'ok', 'running'))).toBe('Step 1 already finished; its copy stays on the hub. Step 2 already finished. Step 3 is running on A and may still finish.')
})

test('only a machine that allows both push and archive can start a move', () => {
  expect(whyNotSource({ remote: { enabled: true, ops: ['push', 'pull'] } })).toBe('archive is off: run asm control enable --allow push,pull,archive there')
  expect(whyNotSource({ remote: { enabled: true, ops: ['pull', 'archive'] } })).toBe('does not allow push')
  expect(whyNotSource({ remote: { enabled: true, ops: ['push', 'pull', 'archive'] } })).toBe('')
  expect(whyNotSource({ remote: { enabled: false } })).toBe('remote control is off')
})

test('a blocked archive that found the session continued is not retried: only a new move can succeed', () => {
  const changed = mstep(3, 'blocked', { code: 'changed_since_move' })
  expect(canRetry(changed)).toBe(false)
  const plan = moveOf('ok', 'ok', ['blocked', { code: 'changed_since_move' }])
  expect(plan.state).toBe('blocked')
  expect(plan.canRetry).toBe(false)
  expect(retryStep(plan.steps)).toBeUndefined()
  expect(resultText(plan.steps[2])).toMatch(/run the move again\.$/)
  expect(resultText(plan.steps[2])).not.toMatch(/Retry|send again/)
  // Any other archive stop can still be retried, and so can an expired archive.
  expect(canRetry(mstep(3, 'blocked', { code: 'live' }))).toBe(true)
  expect(canRetry(mstep(3, 'blocked', { code: 'archived_here' }))).toBe(true)
  expect(canRetry(mstep(3, 'expired', { code: 'expired' }))).toBe(true)
  // A changed push or pull is not an archive: unchanged.
  expect(canRetry(mstep(2, 'blocked', { code: 'changed_since_move' }))).toBe(true)
})

test('hints depend on the step they ended', () => {
  expect(hintFor('unsupported', 'archive')).toBe('That machine does not allow archive commands: run asm control enable --allow push,pull,archive there.')
  expect(hintFor('unsupported', 'pull')).toBe("That machine's asm is too old, or does not run this kind of command.")
  expect(hintFor('unsupported')).toBe(HINTS.unsupported)
  expect(hintFor('live', 'archive')).toMatch(/running on the source\. Close it without continuing it, then Retry\. If you continue it, run the move again\.$/)
  expect(hintFor('live', 'pull')).toBe(HINTS.live)
  expect(hintFor('archived_here', 'pull')).toBe('This machine has the session archived. Restore it there (asm unarchive, or in the agent itself), then Retry.')
  expect(canRetry(step(2, 'blocked', { code: 'archived_here' }))).toBe(true)
  expect(hintFor('diverged', 'pull')).toMatch(/Decide which to keep/)
  expect(hintFor('failed', 'push')).toBe('')
  expect(resultText(mstep(3, 'blocked', { code: 'unsupported' }))).toMatch(/does not allow archive commands/)
  expect(resultText(step(2, 'blocked', { code: 'unsupported' }))).toMatch(/too old/)
})

test('a waiting archive says nothing has been archived', () => {
  const plan = moveOf('running', 'pending', 'pending')
  const byId = Object.fromEntries(plan.steps.map((c) => [c.id, c]))
  expect(resultText(plan.steps[2], byId)).toBe('Not started: A keeps its session until step 2 succeeds; nothing has been archived.')
  // A waiting pull is an ordinary wait.
  expect(resultText(plan.steps[1], byId)).toBe('Waiting for step 1.')
  expect(resultText(mstep(3, 'pending'), {})).toMatch(/until the earlier step succeeds/)
})

test('why a machine cannot be the source or the destination', () => {
  const off = 'archive is off: run asm control enable --allow push,pull,archive there'
  expect(whyNotTarget({ remote: { enabled: true, ops: ['push', 'pull'], v: ARCHIVE_V } }, 'archive')).toBe(off)
  expect(whyNotTarget({ remote: { enabled: true, ops: ['push', 'pull'], v: ARCHIVE_V - 1 } }, 'archive')).toBe('needs a newer asm')
  expect(whyNotTarget({ remote: { enabled: true, ops: ['push', 'pull', 'archive'], v: 2 } }, 'archive')).toBe('')
  expect(whyNotSource({ remote: { enabled: true, ops: ['pull'], v: 2 } })).toBe('does not allow push')
  expect(whyNotTarget({ remote: { enabled: true, ops: ['push'], v: 2 } }, 'pull')).toBe('does not allow pull')
})
