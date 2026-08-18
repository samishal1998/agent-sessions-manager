import { expect, test } from 'bun:test'
import { shortId, shortProject } from './ids.js'

const s = (agent, native_id, slug) => ({ ref: { agent, native_id }, slug })

test('jcode sessions are named by their slug, not the shared prefix', () => {
  // `session_`.slice(0, 8) is the prefix itself — every jcode session
  // rendered identically before this.
  expect(shortId(s('jcode', 'session_bright-canyon', 'bright-canyon'))).toBe('bright-canyon')
})

test('a jcode session with no slug still gets something distinguishing', () => {
  expect(shortId(s('jcode', 'session_a91f77c3e2', null))).toBe('a91f77c3')
})

test('opencode keeps its ses_ prefix, claude is truncated', () => {
  expect(shortId(s('opencode', 'ses_71bc0e4faa2196KmTqRvBnLd2'))).toBe('ses_71bc0e4f')
  expect(shortId(s('claude-code', '7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43'))).toBe('7f3a1c88')
})

test('paths under home abbreviate to ~, on macOS as well as linux', () => {
  expect(shortProject('/home/dev/code/mercury', '/home/dev')).toBe('~/code/mercury')
  expect(shortProject('/Users/dev/code/mercury', '/Users/dev')).toBe('~/code/mercury')
  expect(shortProject('/home/dev', '/home/dev')).toBe('~')
})

test('a path outside home is left alone, and a sibling is not mistaken for it', () => {
  expect(shortProject('/srv/build', '/home/dev')).toBe('/srv/build')
  expect(shortProject('/home/developer/x', '/home/dev')).toBe('/home/developer/x')
})

test('an unknown home leaves paths untouched rather than mangling them', () => {
  expect(shortProject('/home/dev/code', null)).toBe('/home/dev/code')
})
