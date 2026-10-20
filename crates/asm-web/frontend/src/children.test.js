import { expect, test } from 'bun:test'
import { below, deletedWith, flip, groupChildren, keyOf, reveal, rows } from './children.js'

const s = (id, parent = null, updated = null, agent = 'opencode') => ({
  ref: { agent, native_id: id },
  parent,
  updated: updated && new Date(updated * 1000).toISOString(),
})

// The tree as nested ids: `a(b(c) d)`.
const shape = (sessions, nodes) =>
  nodes
    .map((n) => {
      const id = sessions[n.index].ref.native_id
      return n.children.length ? `${id}(${shape(sessions, n.children)})` : id
    })
    .join(' ')

const ids = (list) => list.map((r) => `${'-'.repeat(r.depth)}${r.session.ref.native_id}${r.below && !r.open ? `+${r.below}` : ''}`)

test('children nest to any depth, newest first', () => {
  const all = [s('root', null, 100), s('old', 'root', 10), s('new', 'root', 90), s('grand', 'new', 80), s('great', 'grand', 70), s('other', null, 5)]
  expect(shape(all, groupChildren(all))).toBe('root(new(grand(great)) old) other')
})

test('equal times keep the order given', () => {
  const all = [s('root', null, 9), s('b', 'root', 5), s('a', 'root', 5), s('c', 'root', 5)]
  expect(shape(all, groupChildren(all))).toBe('root(b a c)')
})

test('a child whose parent is not in the list is a root', () => {
  const all = [s('kid', 'ghost', 9), s('root', null, 8), s('sub', 'root', 7)]
  expect(shape(all, groupChildren(all))).toBe('kid root(sub)')
  // A filtered list: the match stands on its own.
  expect(shape([all[2]], groupChildren([all[2]]))).toBe('sub')
})

test('a parent is matched within its own agent', () => {
  const all = [s('same', null, 9, 'claude-code'), s('kid', 'same', 8), s('same', null, 7)]
  expect(shape(all, groupChildren(all))).toBe('same same(kid)')
  expect(groupChildren([all[0], all[1]]).length).toBe(2)
})

test('a cycle is cut at the member that comes first and loses nobody', () => {
  const all = [s('c', 'a', 9), s('a', 'b', 8), s('b', 'a', 7), s('me', 'me', 6)]
  expect(shape(all, groupChildren(all))).toBe('a(c b) me')
  expect(shape(all, groupChildren(all))).toBe(shape(all, groupChildren(all)))
})

test('however the parents point, every session appears once', () => {
  let x = 123456789
  const next = () => (x = (x * 1103515245 + 12345) % 2147483648)
  const n = 300
  const all = Array.from({ length: n }, (_, i) => {
    const pick = next() % 5
    const parent = pick === 0 ? null : pick === 2 ? `s${i}` : pick === 3 ? 'ghost' : `s${next() % n}`
    return s(`s${i}`, parent, next() % 50)
  })
  const seen = rows(all, { tree: true }).map((r) => r.session.ref.native_id).sort()
  expect(seen).toEqual(all.map((a) => a.ref.native_id).sort())
})

test('subagents are folded behind their parent, with how many', () => {
  const all = [s('plan', null, 100), s('review', 'plan', 90), s('nested', 'review', 80), s('explore', 'plan', 70), s('other', null, 60)]
  expect(ids(rows(all))).toEqual(['plan+3', 'other'])
  expect(ids(rows(all, { tree: true }))).toEqual(['plan', '-review', '--nested', '-explore', 'other'])
})

test('a session flipped by hand opens when folded and folds when shown', () => {
  const all = [s('plan', null, 100), s('review', 'plan', 90), s('nested', 'review', 80), s('other', null, 60)]
  const plan = all[0]
  expect(ids(rows(all, { toggled: flip(new Set(), plan) }))).toEqual(['plan', '-review+1', 'other'])
  expect(ids(rows(all, { tree: true, toggled: flip(new Set(), plan) }))).toEqual(['plan+2', 'other'])
})

test('revealing a session opens only what hides it', () => {
  const all = [s('plan', null, 100), s('review', 'plan', 90), s('nested', 'review', 80), s('explore', 'plan', 70), s('other', null, 60)]
  const toggled = reveal(all, all[2], { tree: false })
  expect(ids(rows(all, { toggled }))).toEqual(['plan', '-review', '--nested', '-explore', 'other'])
  // Already shown: nothing flips back.
  expect(reveal(all, all[2], { tree: false, toggled })).toEqual(toggled)
  // In a tree with the parent folded by hand.
  const folded = flip(new Set(), all[0])
  expect(ids(rows(all, { tree: true, toggled: folded }))).toEqual(['plan+3', 'other'])
  expect(ids(rows(all, { tree: true, toggled: reveal(all, all[3], { tree: true, toggled: folded }) }))).toContain('-explore')
  // A loop in the data ends the walk.
  const loop = [s('a', 'b'), s('b', 'a')]
  expect(reveal(loop, loop[0], { tree: false }).size).toBeLessThanOrEqual(2)
})

test('what is under a session, with depths, in tree order', () => {
  const all = [s('a', null, 9), s('b', 'a', 8), s('c', 'b', 7), s('d', 'a', 6), s('z', null, 5)]
  const names = (x) => below(all, x).map((r) => `${r.depth}${r.session.ref.native_id}`)
  expect(names(all[0])).toEqual(['1b', '2c', '1d'])
  expect(names(all[1])).toEqual(['1c'])
  expect(names(all[4])).toEqual([])
})

test('a delete counts what it takes along, for the agents that take it', () => {
  const all = [s('plan', null, 9), s('review', 'plan', 8), s('nested', 'review', 7), s('explore', 'plan', 6)]
  expect(deletedWith(all, [all[0]])).toBe(3)
  expect(deletedWith(all, [all[0], all[1]])).toBe(2)
  expect(deletedWith(all, [all[2]])).toBe(0)
  const jcode = [s('main', null, 9, 'jcode'), s('kid', 'main', 8, 'jcode')]
  expect(deletedWith(jcode, [jcode[0]])).toBe(0)
})

test('keys name an agent and an id', () => {
  expect(keyOf(s('x'))).not.toBe(keyOf(s('x', null, null, 'jcode')))
})
