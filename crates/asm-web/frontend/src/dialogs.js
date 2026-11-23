import { reactive } from 'vue'

// Promise-based replacements for window.confirm / window.prompt, rendered by
// <DialogHost> as Hearth dialogs (focus-trapped, themed, keyboard-closable).
//
//   if (await confirmDialog({ title: 'Delete?', description: '…', confirmLabel: 'Delete', danger: true })) …
//   const dir = await promptDialog({ title: 'Move to', label: 'Directory', value: '/a' })   // string | null
//   const v = await formDialog({ title: 'Hub', fields: [{ name: 'url', label: 'URL', value: '' }] })   // { url } | null
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

// Several fields at once: `fields` is [{ name, label, value, hint, type, placeholder }];
// the answer is { [name]: text } or null. `error` shows above the fields.
export const formDialog = ({ title, description = '', error = '', fields, confirmLabel = 'OK' }) =>
  open({ kind: 'form', title, description, error, fields, confirmLabel })
