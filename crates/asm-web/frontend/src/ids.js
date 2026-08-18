// Display helpers that must agree with asm-core. `shortId` mirrors
// `Session::short_id` in crates/asm-core/src/model/session.rs — when that
// changes, this and ids.test.js change with it.

/// jcode names sessions `session_<memorable-name>`, so the first eight
/// characters of the native id are the constant prefix and identify
/// nothing; its slug is the part a human recognises.
export function shortId(session) {
  const id = session.ref.native_id
  if (session.ref.agent === 'jcode' && session.slug) return session.slug
  const trimmed = id.startsWith('session_') ? id.slice('session_'.length) : id
  return id.startsWith('ses_') ? id.slice(0, 12) : trimmed.slice(0, 8)
}

/// Abbreviate a path under the user's home to `~`. The home directory comes
/// from the server (/api/meta): it is `/Users/<name>` on macOS, not
/// `/home/<user>`, and may be anywhere at all when HOME is set explicitly.
export function shortProject(root, home) {
  if (home && (root === home || root.startsWith(`${home}/`))) {
    return `~${root.slice(home.length)}`
  }
  return root
}
