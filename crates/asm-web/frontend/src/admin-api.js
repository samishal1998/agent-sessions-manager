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

async function call(token, method, path, payload, root = '/hub/v1/admin') {
  const headers = { Authorization: `Bearer ${token}` }
  if (payload !== undefined) headers['Content-Type'] = 'application/json'
  const r = await fetch(`${root}${path}`, { method, headers, body: payload === undefined ? undefined : JSON.stringify(payload) })
  const body = await r.json().catch(() => ({}))
  if (!r.ok) throw new AdminError(r.status, body.error || `${r.status}`)
  return body
}

export const admin = (token) => ({
  overview: () => call(token, 'GET', '/overview'),
  machines: () => call(token, 'GET', '/machines'),
  sessions: () => call(token, 'GET', '/sessions'),
  transcript: (agent, id) => call(token, 'GET', `/sessions/${encodeURIComponent(agent)}/${encodeURIComponent(id)}/transcript`),
  log: () => call(token, 'GET', '/log'),
  revoke: (id) => call(token, 'POST', `/machines/${encodeURIComponent(id)}/revoke`),
  rotateJoin: () => call(token, 'POST', '/join-token/rotate'),
  remove: (agent, id) => call(token, 'DELETE', `/sessions/${encodeURIComponent(agent)}/${encodeURIComponent(id)}`),
  // Remote control: the commands queue lives beside /admin, not under it.
  commands: (limit = 100) => call(token, 'GET', `?limit=${limit}`, undefined, '/hub/v1/commands'),
  command: (id) => call(token, 'GET', `/${encodeURIComponent(id)}`, undefined, '/hub/v1/commands'),
  createCommand: (body) => call(token, 'POST', '', body, '/hub/v1/commands'),
  cancelCommand: (id) => call(token, 'POST', `/${encodeURIComponent(id)}/cancel`, undefined, '/hub/v1/commands'),
  retryCommand: (id) => call(token, 'POST', `/${encodeURIComponent(id)}/retry`, undefined, '/hub/v1/commands'),
  collect: (dryRun) => call(token, 'POST', `/collect?dry_run=${dryRun ? 'true' : 'false'}`),
})
