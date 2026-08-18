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
}
