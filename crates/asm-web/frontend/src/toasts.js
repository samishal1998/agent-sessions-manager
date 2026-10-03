import { reactive } from 'vue'

// Notifications for <HToaster>. `notify('Archived 2 sessions')`;
// `notify(msg, { tone: 'danger', duration: 0 })` stays until dismissed.
export const toasts = reactive([])
let n = 0
export function notify(title, { tone = 'neutral', description = '', duration } = {}) {
  const id = `t${++n}`
  // Errors persist until dismissed; routine messages go on their own.
  toasts.push({ id, title, description, tone, duration: duration ?? (tone === 'danger' ? 0 : 5000) })
  return id
}
export const dismiss = (id) => {
  const i = toasts.findIndex((t) => t.id === id)
  if (i >= 0) toasts.splice(i, 1)
}
