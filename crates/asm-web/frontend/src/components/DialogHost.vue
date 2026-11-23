<script setup>
import { reactive, ref, watch } from 'vue'
import { HAlert, HButton, HDialog, HInput } from '@hearth-ui/vue'
import { dialogState } from '../dialogs.js'

// Mount once, inside the theme island. Answers the promise a caller is
// awaiting: true/false for confirm, the text or null for prompt, the fields'
// values or null for a form.
const text = ref('')
const values = reactive({})
watch(
  () => dialogState.current,
  (d) => {
    text.value = d?.value ?? ''
    for (const k of Object.keys(values)) delete values[k]
    for (const f of d?.fields || []) values[f.name] = f.value ?? ''
  },
)
function answer(value) {
  const d = dialogState.current
  if (!d) return
  dialogState.current = null
  d.resolve(value)
}
const ANSWER = { prompt: () => text.value, form: () => ({ ...values }) }
const ok = () => answer(ANSWER[dialogState.current.kind]?.() ?? true)
const cancel = () => answer(dialogState.current.kind === 'confirm' ? false : null)
</script>

<template>
  <HDialog v-if="dialogState.current" :open="true" :title="dialogState.current.title" :description="dialogState.current.description" @close="cancel">
    <form class="dialog-form" @submit.prevent="ok">
      <HInput v-if="dialogState.current.kind === 'prompt'" v-model="text" :label="dialogState.current.label" />
      <template v-if="dialogState.current.kind === 'form'">
        <HAlert v-if="dialogState.current.error" tone="danger" :description="dialogState.current.error" />
        <HInput
          v-for="f in dialogState.current.fields"
          :key="f.name"
          v-model="values[f.name]"
          :label="f.label"
          :hint="f.hint"
          :type="f.type || 'text'"
          :placeholder="f.placeholder"
          autocomplete="off"
        />
      </template>
      <div class="dialog-actions">
        <HButton type="button" variant="ghost" @click="cancel">Cancel</HButton>
        <HButton type="submit" :variant="dialogState.current.danger ? 'danger' : 'primary'">{{ dialogState.current.confirmLabel }}</HButton>
      </div>
    </form>
  </HDialog>
</template>
