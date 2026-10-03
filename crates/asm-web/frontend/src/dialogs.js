import { reactive } from 'vue'

// Promise-based replacements for window.confirm / window.prompt, rendered by
// <DialogHost> as Hearth dialogs (focus-trapped, themed, keyboard-closable).
//
//   if (await confirmDialog({ title: 'Delete?', description: '…', confirmLabel: 'Delete', danger: true })) …
//   const dir = await promptDialog({ title: 'Move to', label: 'Directory', value: '/a' })   // string | null
export const dialogState = reactive({ current: null })

function open(spec) {
  return new Promise((resolve) => {
    // One at a time: a second request waits its turn rather than replacing
    // the first, whose answer would be lost.
    const show = () => (dialogState.current = { ...spec, resolve })
    if (!dialogState.current) return show()
    const prev = dialogState.current.resolve
    dialogState.current.resolve = (v) => {
      prev(v)
      show()
    }
  })
}

export const confirmDialog = ({ title, description = '', confirmLabel = 'OK', danger = false }) =>
  open({ kind: 'confirm', title, description, confirmLabel, danger })

export const promptDialog = ({ title, description = '', label, value = '', confirmLabel = 'OK' }) =>
  open({ kind: 'prompt', title, description, label, value, confirmLabel })
