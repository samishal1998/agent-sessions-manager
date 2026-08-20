async function request(path, options) {
  const response = await fetch(path, options)
  const body = await response.json().catch(() => ({}))
  if (!response.ok) {
    throw new Error(body.error || `${response.status} ${response.statusText}`)
  }
  return body
}

const post = (path, body) =>
  request(path, {
    method: 'POST',
    // X-Asm-Request marks this as a real request from the asm UI: a
    // cross-origin form POST cannot set it, and adding it forces a
    // preflight the server does not answer.
    headers: { 'Content-Type': 'application/json', 'X-Asm-Request': '1' },
    body: JSON.stringify(body ?? {}),
  })

const sessionPath = (s, action) =>
  `/api/session/${encodeURIComponent(s.ref.agent)}/${encodeURIComponent(s.ref.native_id)}/${action}`

export default {
  meta: () => request('/api/meta'),
  // One verb over many sessions. `action` is the tagged BulkAction the
  // core defines: {action:'archive'} | {action:'move',dir} | …
  bulk: (sessions, action) =>
    post('/api/bulk', {
      sessions: sessions.map((s) => ({ agent: s.ref.agent, native_id: s.ref.native_id })),
      ...action,
    }),
  sessions: (all) => request(`/api/sessions?all=${all ? 'true' : 'false'}`),
  projects: () => request('/api/projects'),
  doctor: () => request('/api/doctor'),
  search: (q, agent, project) => {
    const params = new URLSearchParams({ q })
    if (agent) params.set('agent', agent)
    if (project) params.set('project', project)
    return request(`/api/search?${params}`)
  },
  indexStats: () => request('/api/index'),
  indexRefresh: () => post('/api/index/refresh'),
  ir: (s) => request(sessionPath(s, 'ir')),
  rename: (s, title) => post(sessionPath(s, 'rename'), { title }),
  archive: (s) => post(sessionPath(s, 'archive')),
  unarchive: (s) => post(sessionPath(s, 'unarchive')),
  remove: (s) => post(sessionPath(s, 'delete')),
  move: (s, dir) => post(sessionPath(s, 'move'), { dir }),
  import: (s, to, seed) => post(sessionPath(s, 'import'), { to, seed }),

  // Send a message into a session and hand back each normalized event as
  // it arrives. The reply is an NDJSON stream rather than JSON, so this one
  // cannot go through `request`, which waits for the whole body.
  //
  // Not EventSource: it cannot set the X-Asm-Request header the server
  // requires, and this is a mutation — it spends tokens and lets the agent
  // edit files — so it must not become a GET to suit the browser API.
  //
  // Returns an abort function; calling it drops the connection, which the
  // server reads as "stop the turn" and kills the agent process.
  send(s, message, onEvent) {
    const controller = new AbortController()
    const done = (async () => {
      const response = await fetch(sessionPath(s, 'send'), {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-Asm-Request': '1' },
        body: JSON.stringify({ message }),
        signal: controller.signal,
      })
      if (!response.ok) {
        const body = await response.json().catch(() => ({}))
        throw new Error(body.error || `${response.status} ${response.statusText}`)
      }
      const reader = response.body.getReader()
      const decoder = new TextDecoder()
      // A chunk boundary lands anywhere, including mid-line; whatever is
      // left over after the last newline is the start of the next event.
      let buffer = ''
      for (;;) {
        const { done: finished, value } = await reader.read()
        if (finished) break
        buffer += decoder.decode(value, { stream: true })
        const lines = buffer.split('\n')
        buffer = lines.pop() ?? ''
        for (const line of lines) {
          if (!line.trim()) continue
          try {
            onEvent(JSON.parse(line))
          } catch {
            /* a half-written line is not worth failing the stream over */
          }
        }
      }
    })()
    return { done, abort: () => controller.abort() }
  },
}
