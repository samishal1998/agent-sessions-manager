import { computed, reactive, ref } from 'vue'
import { post, request } from './api.js'

// Work the server runs in the background — push, pull, compare and the bulk
// verbs — as it reports it. The server keeps the last 50, newest first, and
// forgets them on a restart. They are asked for every second while one is
// queued or running, every 15s otherwise, and at once after starting one.
export const tasks = ref([])
// The task panel: whether it is open, and the task to point at in it.
export const panel = reactive({ open: false, highlight: null })

export const isOpen = (t) => t.state === 'queued' || t.state === 'running'
export const busy = (key) => tasks.value.some((t) => t.key === key && isOpen(t))
export const busyAny = computed(() => tasks.value.some(isOpen))
export const busyUnder = (prefix) => tasks.value.some((t) => t.key.startsWith(prefix) && isOpen(t))
// Whether an open task under `prefix` (`bulk:push:`) covers the session
// `ref` (`agent:id`): a bulk key ends in its sessions, joined by commas.
export const covers = (prefix, ref) =>
  tasks.value.some((t) => isOpen(t) && t.key.startsWith(prefix) && t.key.slice(prefix.length).split(',').includes(ref))

// Tasks someone is awaiting, by id.
const waiting = new Map()
let timer = null
let polling = false
let again = false

export async function poll() {
  if (polling) return void (again = true)
  polling = true
  clearTimeout(timer)
  try {
    const list = (await request('/api/tasks')).tasks || []
    tasks.value = list
    for (const [id, waiters] of waiting) {
      // Off the end of the list: ask for it alone. A 404 means it is gone.
      const job =
        list.find((t) => t.id === id) ||
        (await request(`/api/tasks/${id}`).catch((e) => ({ id, state: 'failed', error: `the task was lost: ${e.message}` })))
      if (isOpen(job)) continue
      waiting.delete(id)
      for (const w of waiters)
        job.state === 'ok' ? w.resolve(job.result) : w.reject(Object.assign(new Error(job.error || 'failed'), { job }))
    }
  } catch {
    /* the server did not answer; the next poll asks again */
  }
  polling = false
  timer = setTimeout(poll, again ? 0 : busyAny.value || waiting.size ? 1000 : 15000)
  again = false
}

async function begin(path, body) {
  const answer = await post(`${path}?async=1`, body)
  // The same work is already under way: point at it rather than start more.
  if (answer.existing) Object.assign(panel, { open: true, highlight: answer.job })
  poll()
  return answer
}

/// Start a task; resolves with its id.
export const start = async (path, body) => (await begin(path, body)).job

/// Resolves with the task's result when it ends well, rejects with its error
/// (the same text the synchronous route would have answered) when it fails.
export const finished = (id) =>
  new Promise((resolve, reject) => waiting.set(id, [...(waiting.get(id) || []), { resolve, reject }]))

/// Start a task and wait for it. Null when the same work was already running:
/// whoever started that one reports it.
export async function run(path, body) {
  const { job, existing } = await begin(path, body)
  return existing ? null : finished(job)
}
