// Child sessions (subagents) as a tree. The rules are asm-core's `ops::group_children`
// (crates/asm-core/src/ops.rs) on purpose, so the TUI, the CLI and this page
// agree on what is under what; when one changes, change both.

export const keyOf = (s) => `${s.ref.agent}\u0000${s.ref.native_id}`

// OpenCode deletes a session's whole tree with it; the other agents' sessions
// are deleted alone. Mirrors `ops::deletes_descendants`.
export const deletesDescendants = (agent) => agent === 'opencode'

/// The sessions as a forest of `{ index, children }`, `index` being a position
/// in `sessions`.
///
/// - Roots stay in the order given; children go newest first (the sort is
///   stable, so equal times keep the order given).
/// - A parent is looked up by agent and id together: ids are only unique within
///   an agent.
/// - A session whose parent is not in the list is a root: an orphan, or a match
///   in a filtered list whose parent did not match.
/// - A cycle in the data is cut at its member that comes first, which becomes a
///   root, so every session appears exactly once.
export function groupChildren(sessions) {
  const n = sessions.length
  const at = new Map()
  sessions.forEach((s, i) => {
    const k = keyOf(s)
    if (!at.has(k)) at.set(k, i)
  })
  const parentOf = sessions.map((s, i) => {
    if (!s.parent) return null
    const p = at.get(`${s.ref.agent}\u0000${s.parent}`)
    return p === undefined || p === i ? null : p
  })
  const kids = Array.from({ length: n }, () => [])
  parentOf.forEach((p, child) => {
    if (p !== null) kids[p].push(child)
  })
  for (const k of kids) k.sort((a, b) => (sessions[b].updated || '').localeCompare(sessions[a].updated || ''))

  const seen = new Array(n).fill(false)
  const grow = (i) => {
    seen[i] = true
    const children = []
    for (const c of kids[i]) if (!seen[c]) children.push(grow(c))
    return { index: i, children }
  }
  const roots = []
  for (let i = 0; i < n; i++) if (parentOf[i] === null) roots.push(grow(i))
  // What is left hangs off a cycle: no root reaches it.
  for (let start = 0; start < n; start++) {
    if (seen[start]) continue
    const path = [start]
    let cursor = start
    let cycle = path
    for (;;) {
      const next = parentOf[cursor]
      if (next === null) break
      const found = path.indexOf(next)
      if (found >= 0) {
        cycle = path.slice(found)
        break
      }
      path.push(next)
      cursor = next
    }
    roots.push(grow(Math.min(...cycle)))
  }
  return roots.sort((a, b) => a.index - b.index)
}

/// Sessions below a node, at any depth.
export const descendants = (node) => node.children.reduce((sum, c) => sum + 1 + descendants(c), 0)

/// The tree in display order: `{ index, depth, below, open }`, where `below`
/// counts everything under it and `open` says whether those are shown.
/// `isOpen(index)` decides, per session.
export function flatten(nodes, isOpen) {
  const out = []
  const walk = (list, depth) => {
    for (const node of list) {
      const below = descendants(node)
      const open = below > 0 && isOpen(node.index)
      out.push({ index: node.index, depth, below, open })
      if (open) walk(node.children, depth + 1)
    }
  }
  walk(nodes, 0)
  return out
}

/// The rows of the list. `tree` shows every subagent under its parent; without
/// it they are folded behind it. `toggled` holds the sessions flipped by hand
/// from that: opened when folded by default, folded when shown.
export function rows(sessions, { tree = false, toggled = new Set() } = {}) {
  const forest = groupChildren(sessions)
  return flatten(forest, (i) => tree !== toggled.has(keyOf(sessions[i]))).map((r) => ({
    session: sessions[r.index],
    depth: r.depth,
    below: r.below,
    open: r.open,
  }))
}

/// Everything under `session`, in tree order, each with its depth below it.
export function below(sessions, session) {
  const flat = flatten(groupChildren(sessions), () => true)
  const key = keyOf(session)
  const at = flat.findIndex((r) => keyOf(sessions[r.index]) === key)
  if (at < 0) return []
  const out = []
  for (let i = at + 1; i < flat.length && flat[i].depth > flat[at].depth; i++) {
    out.push({ session: sessions[flat[i].index], depth: flat[i].depth - flat[at].depth })
  }
  return out
}

/// How many sessions deleting `batch` takes along without their being in it:
/// those under a session of an agent that deletes its tree.
export function deletedWith(sessions, batch) {
  const named = new Set(batch.map(keyOf))
  const flat = flatten(groupChildren(sessions), () => true)
  const taken = new Set()
  flat.forEach((r, at) => {
    const s = sessions[r.index]
    if (!named.has(keyOf(s)) || !deletesDescendants(s.ref.agent)) return
    for (let i = at + 1; i < flat.length && flat[i].depth > r.depth; i++) taken.add(keyOf(sessions[flat[i].index]))
  })
  return [...taken].filter((k) => !named.has(k)).length
}

/// `toggled` with whatever hides `session` flipped open, so its row exists.
/// Walks up through its parents; a loop in the data ends the walk.
export function reveal(sessions, session, { tree = false, toggled = new Set() } = {}) {
  const byKey = new Map(sessions.map((s) => [keyOf(s), s]))
  const next = new Set(toggled)
  const visited = new Set([keyOf(session)])
  let at = session
  while (at.parent) {
    const parent = byKey.get(`${at.ref.agent}\u0000${at.parent}`)
    if (!parent || visited.has(keyOf(parent))) break
    visited.add(keyOf(parent))
    const k = keyOf(parent)
    // Shown already when the global choice and the flip disagree.
    if (!(tree !== next.has(k))) next.has(k) ? next.delete(k) : next.add(k)
    at = parent
  }
  return next
}

/// A copy of `toggled` with `session` flipped.
export function flip(toggled, session) {
  const next = new Set(toggled)
  const k = keyOf(session)
  next.has(k) ? next.delete(k) : next.add(k)
  return next
}
