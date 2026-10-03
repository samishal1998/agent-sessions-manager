// How the hub view writes times and sizes.

/// "just now", "5m ago", "3h ago", "2d ago" for an ISO timestamp or Date.
export function ago(ts, now = Date.now()) {
  if (!ts) return ''
  const seconds = Math.max(0, (now - new Date(ts).getTime()) / 1000)
  if (seconds < 60) return 'just now'
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`
  return `${Math.floor(seconds / 86400)}d ago`
}

/// The same for unix seconds, which is what the daemon's status file holds.
export const agoUnix = (secs, now = Date.now()) => (secs ? ago(secs * 1000, now) : '')

export function bytes(n) {
  if (n == null) return ''
  const units = ['B', 'KB', 'MB', 'GB']
  let v = n
  let u = 0
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024
    u++
  }
  return u === 0 ? `${v} B` : `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[u]}`
}

/// The badge tone for a sync state: green when level, the accent when this
/// machine has something to send or get, amber when it needs a decision, and
/// blue for what only the hub has.
export const hubTone = (state) =>
  ({ in_sync: 'success', ahead: 'accent', local: 'accent', behind: 'accent', diverged: 'warning', untracked: 'warning', remote: 'info' })[state] || 'neutral'
