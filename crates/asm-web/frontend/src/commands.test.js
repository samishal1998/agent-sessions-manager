import { expect, test } from 'bun:test'
import { HINTS, remoteStatus, resultText, whyNotTarget } from './commands.js'

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
