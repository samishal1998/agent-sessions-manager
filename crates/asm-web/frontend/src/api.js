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

const api = {
  meta: () => request('/api/meta'),
  // One verb over many sessions. `action` is the tagged BulkAction the
  // core defines: {action:'archive'} | {action:'move',dir} | …
  bulk: (sessions, action) =>
    post('/api/bulk', {
      sessions: sessions.map((s) => ({ agent: s.ref.agent, native_id: s.ref.native_id })),
      ...action,
    }),
  sessions: (all) => request(`/api/sessions?all=${all ? 'true' : 'false'}`),

  // Sessions as the stores give them up, one JSON line per batch, so a
  // machine with thousands of them draws the first rows at once instead of
  // showing nothing until the last store has been read. `onBatch` is called
  // per batch; the promise resolves with whatever could not be read. A
  // server or browser without streaming falls back to the whole list.
  async sessionsStreamed(all, onBatch) {
    const response = await fetch(`/api/sessions?all=${all ? 'true' : 'false'}&stream=1`)
    if (!response.ok || !response.body) {
      onBatch(await api.sessions(all))
      return []
    }
    const reader = response.body.getReader()
    const decoder = new TextDecoder()
    let buffer = ''
    let problems = []
    let finished = false
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      buffer += decoder.decode(value, { stream: true })
      // A chunk boundary lands anywhere, including mid-line.
      let newline
      while ((newline = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, newline)
        buffer = buffer.slice(newline + 1)
        if (!line.trim()) continue
        const message = JSON.parse(line)
        // Awaited, so a caller can yield to the browser between batches.
        if (message.sessions) await onBatch(message.sessions)
        else {
          problems = message.problems || []
          finished = true
        }
      }
    }
    // A body that stops early looks exactly like a complete one, and the
    // rows that did arrive would be shown as the whole store.
    if (!finished) throw new Error('the list of sessions ended early, so some may be missing')
    return problems
  },
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
  // The hub this machine joined, with every session here and there side by
  // side; {joined:false} when there is none.
  hub: () => request('/api/hub'),
  push: (sessions) => api.bulk(sessions, { action: 'push' }),
  pullAll: () => post('/api/hub/pull-all', {}),
  // Compare a session with the hub's copy, installing nothing.
  compare: (agent, id) => post('/api/hub/compare', { agent, id }),
  pull: (agent, id, projectDir) =>
    post('/api/hub/pull', { agent, id, project_dir: projectDir || null }),
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

export default api
