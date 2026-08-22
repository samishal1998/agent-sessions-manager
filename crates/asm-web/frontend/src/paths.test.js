import { expect, test } from 'bun:test'
import { uniqueTails } from './paths.js'

test('a single project is named by its own directory', () => {
  const tails = uniqueTails(['~/code/mercury'])
  expect(tails.get('~/code/mercury')).toBe('mercury')
})

test('colliding names grow a segment, and only the ones that collide', () => {
  const tails = uniqueTails(['~/home/project', '~/game/project', '~/code/mercury'])
  expect(tails.get('~/home/project')).toBe('home/project')
  expect(tails.get('~/game/project')).toBe('game/project')
  // mercury never collided, so it must not have grown in sympathy.
  expect(tails.get('~/code/mercury')).toBe('mercury')
})

test('a collision that survives one segment grows again', () => {
  const tails = uniqueTails(['~/a/shared/web', '~/b/shared/web', '~/c/other/web'])
  expect(tails.get('~/a/shared/web')).toBe('a/shared/web')
  expect(tails.get('~/b/shared/web')).toBe('b/shared/web')
  // This one was distinguished by the second segment alone.
  expect(tails.get('~/c/other/web')).toBe('other/web')
})

/// The loop has to stop even when growing cannot separate two paths.
test('a path that is a tail of another terminates', () => {
  const tails = uniqueTails(['project', '~/code/project'])
  expect(tails.get('project')).toBe('project')
  // Grown as far as it can go, which is still enough to tell them apart.
  expect(tails.get('~/code/project')).toBe('code/project')
})

test('duplicates and empty paths do not produce entries', () => {
  const tails = uniqueTails(['~/code/mercury', '~/code/mercury', '', null, undefined])
  expect(tails.size).toBe(1)
  expect(tails.get('~/code/mercury')).toBe('mercury')
})

test('a root path is named by itself rather than by nothing', () => {
  const tails = uniqueTails(['/'])
  expect(tails.get('/')).toBe('/')
})

test('an empty list is not an error', () => {
  expect(uniqueTails([]).size).toBe(0)
})

/// `~` is not a segment worth growing into: it made a shallow path's label
/// keep the home marker while its deeper sibling dropped it.
test('the home marker never appears in a label', () => {
  const tails = uniqueTails(['~/work/mercury', '~/archive/work/mercury'])
  expect(tails.get('~/work/mercury')).toBe('work/mercury')
  expect(tails.get('~/archive/work/mercury')).toBe('archive/work/mercury')
  for (const label of tails.values()) expect(label).not.toContain('~')
})

test('home itself is still named something', () => {
  const tails = uniqueTails(['~'])
  expect(tails.get('~')).toBe('~')
})
