// The hub's admin API. The token travels in an Authorization header, never a
// cookie, so no other web page can make a request that carries it.
const KEY = 'asm-hub-admin-token'

export const savedToken = () => {
  try {
    return sessionStorage.getItem(KEY) || ''
  } catch {
    return ''
  }
}
export const saveToken = (t) => {
  try {
    t ? sessionStorage.setItem(KEY, t) : sessionStorage.removeItem(KEY)
  } catch {
    /* a private window: the token lives only as long as the page */
  }
}

export class AdminError extends Error {
  constructor(status, message) {
    super(message)
    this.status = status
  }
}

async function call(token, method, path) {
  const r = await fetch(`/hub/v1/admin${path}`, { method, headers: { Authorization: `Bearer ${token}` } })
  const body = await r.json().catch(() => ({}))
  if (!r.ok) throw new AdminError(r.status, body.error || `${r.status}`)
  return body
}

export const admin = (token) => ({
  overview: () => call(token, 'GET', '/overview'),
  machines: () => call(token, 'GET', '/machines'),
  sessions: () => call(token, 'GET', '/sessions'),
  log: () => call(token, 'GET', '/log'),
  revoke: (id) => call(token, 'POST', `/machines/${encodeURIComponent(id)}/revoke`),
  rotateJoin: () => call(token, 'POST', '/join-token/rotate'),
  remove: (agent, id) => call(token, 'DELETE', `/sessions/${encodeURIComponent(agent)}/${encodeURIComponent(id)}`),
  collect: (dryRun) => call(token, 'POST', `/collect?dry_run=${dryRun ? 'true' : 'false'}`),
})
