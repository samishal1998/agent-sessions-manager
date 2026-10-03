<script setup>
import { ref, watch } from 'vue'
import { HButton, HDialog, HInput } from '@hearth-ui/vue'
import { dialogState } from '../dialogs.js'

// Mount once, inside the theme island. Answers the promise a caller is
// awaiting: true/false for confirm, the text or null for prompt.
const text = ref('')
watch(
  () => dialogState.current,
  (d) => (text.value = d?.value ?? ''),
)
function answer(value) {
  const d = dialogState.current
  if (!d) return
  dialogState.current = null
  d.resolve(value)
}
const ok = () => answer(dialogState.current.kind === 'prompt' ? text.value : true)
const cancel = () => answer(dialogState.current.kind === 'prompt' ? null : false)
</script>

<template>
  <HDialog v-if="dialogState.current" :open="true" :title="dialogState.current.title" :description="dialogState.current.description" @close="cancel">
    <form class="dialog-form" @submit.prevent="ok">
      <HInput v-if="dialogState.current.kind === 'prompt'" v-model="text" :label="dialogState.current.label" />
      <div class="dialog-actions">
        <HButton type="button" variant="ghost" @click="cancel">Cancel</HButton>
        <HButton type="submit" :variant="dialogState.current.danger ? 'danger' : 'primary'">{{ dialogState.current.confirmLabel }}</HButton>
      </div>
    </form>
  </HDialog>
</template>
