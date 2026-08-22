// Turning a set of project directories into labels short enough to scan.
//
// `shortProject` lives in ids.js beside the session-id rules; this is the
// separate question of what to *call* a project once its path is known.

function segments(path) {
  const parts = path.split('/').filter(Boolean)
  // `~` is not part of what anyone calls a project. Counting it made the
  // label of a shallow path grow into `~/work/mercury` while a deeper
  // sibling stopped at `archive/work/mercury`, so one row kept the home
  // marker and the other did not — and the shallow one then matched its own
  // subtitle exactly and lost it.
  return parts[0] === '~' ? parts.slice(1) : parts
}

function suffix(path, depth) {
  const parts = segments(path)
  if (!parts.length) return path
  const kept = parts.slice(Math.max(parts.length - depth, 0))
  return kept.join('/')
}

/// The shortest tail of each path that tells it apart from the others.
///
/// One `project` stays `project`; two of them become `home/project` and
/// `game/project`, and only the ones that actually collide grow. The full
/// path is still shown underneath, so this is about what the eye lands on
/// first, not about being unambiguous on its own.
///
/// Returns a Map of path -> label.
export function uniqueTails(paths) {
  const roots = [...new Set(paths.filter((p) => p))]
  const depth = new Map(roots.map((p) => [p, 1]))

  // Grow colliding labels a segment at a time. Each pass either separates
  // some group or leaves every member of it at its full length, so the
  // maximum segment count bounds the work.
  const maxSegments = Math.max(1, ...roots.map((p) => segments(p).length))
  for (let pass = 0; pass < maxSegments; pass += 1) {
    const groups = new Map()
    for (const root of roots) {
      const key = suffix(root, depth.get(root))
      if (!groups.has(key)) groups.set(key, [])
      groups.get(key).push(root)
    }
    let grew = false
    for (const members of groups.values()) {
      if (members.length < 2) continue
      for (const root of members) {
        // Already showing everything it has; a longer suffix does not
        // exist, so two paths that differ only above the root — or a path
        // that is a tail of another — stop here rather than spinning.
        if (depth.get(root) >= segments(root).length) continue
        depth.set(root, depth.get(root) + 1)
        grew = true
      }
    }
    if (!grew) break
  }

  return new Map(roots.map((root) => [root, suffix(root, depth.get(root))]))
}
