import { expect, test } from 'bun:test'
import { busy, covers, panel, run, tasks } from './tasks.js'

// A server that starts job 7 (or says it already runs), then lists it running, then done or failed.
function server({ existing = false, end = { state: 'ok', result: { summary: 'pushed 2' } } } = {}) {
  let polls = 0
  const job = { id: 7, key: 'bulk:push:claude-code:a,opencode:b', state: 'running', label: 'Push 2' }
  globalThis.fetch = async (path) => {
    const body = path.includes('async=1')
      ? { job: 7, ...(existing ? { existing: true } : {}) }
      : { tasks: [++polls < 2 ? job : { ...job, ...end }] }
    return { ok: true, json: async () => body }
  }
}

test('a task resolves with its result once the server says it is done', async () => {
  server()
  expect(await run('/api/bulk', {})).toEqual({ summary: 'pushed 2' })
})

test('a failed task rejects with the error the route would have answered', async () => {
  server({ end: { state: 'failed', error: 'pass --project-dir' } })
  await expect(run('/api/hub/pull', {})).rejects.toThrow('--project-dir')
})

test('a second start of running work starts nothing and points at it', async () => {
  server({ existing: true })
  expect(await run('/api/bulk', {})).toBeNull()
  expect(panel).toEqual({ open: true, highlight: 7 })
})

test('busy and covers read the keys', () => {
  tasks.value = [{ id: 1, key: 'bulk:push:claude-code:a,opencode:b', state: 'queued' }, { id: 2, key: 'pull-all', state: 'ok' }]
  expect(covers('bulk:push:', 'opencode:b')).toBe(true)
  expect(covers('bulk:push:', 'opencode:c')).toBe(false)
  expect(busy('pull-all')).toBe(false)
})
