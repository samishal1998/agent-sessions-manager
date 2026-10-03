import { expect, test } from 'bun:test'
import { ago, agoUnix, bytes } from './format.js'

test('ago reads in the largest sensible unit', () => {
  const now = Date.parse('2026-10-03T12:00:00Z')
  expect(ago('2026-10-03T11:59:50Z', now)).toBe('just now')
  expect(ago('2026-10-03T11:55:00Z', now)).toBe('5m ago')
  expect(ago('2026-10-03T09:00:00Z', now)).toBe('3h ago')
  expect(ago('2026-10-01T12:00:00Z', now)).toBe('2d ago')
  expect(ago(null, now)).toBe('')
})

test('daemon times are unix seconds', () => {
  const now = 1_000_000 * 1000
  expect(agoUnix(1_000_000 - 120, now)).toBe('2m ago')
  expect(agoUnix(0, now)).toBe('')
})

test('sizes', () => {
  expect(bytes(512)).toBe('512 B')
  expect(bytes(1536)).toBe('1.5 KB')
  expect(bytes(184 * 1024)).toBe('184 KB')
  expect(bytes(null)).toBe('')
})
