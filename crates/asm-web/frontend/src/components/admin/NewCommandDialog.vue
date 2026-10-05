<script setup>
import { computed, ref, watch } from 'vue'
import { HAlert, HButton, HCheckbox, HCombobox, HDialog, HSegmentedControl, HSelect } from '@hearth-ui/vue'
import { AdminError } from '../../admin-api.js'
import { whyNotTarget } from '../../commands.js'
import { shortId } from '../../ids.js'

// Ask one machine to push or pull one session. Mounted only while open, so
// every opening starts from a clean form.
const props = defineProps({
  sessions: { type: Array, required: true },
  machines: { type: Array, required: true },
  api: { type: Object, required: true },
  prefill: { type: Object, default: null }, // a hub session: from "Send to machine…"
})
const emit = defineEmits(['close', 'created', 'expired'])

const key = (s) => `${s.agent}/${s.id}`
const op = ref(props.prefill ? 'pull' : 'push')
const picked = ref(props.prefill ? key(props.prefill) : '')
const machine = ref('')
const from = ref('')
const exact = ref(false)
const error = ref('')
const busy = ref(false)

const byKey = computed(() => Object.fromEntries(props.sessions.map((s) => [key(s), s])))
const session = computed(() => byKey.value[picked.value] || null)
const sid = (s) => shortId({ ref: { agent: s.agent, native_id: s.id }, slug: s.slug })
const sessionOptions = computed(() => props.sessions.map((s) => ({
  value: key(s), label: s.title || 'Untitled session',
  description: `${s.agent} · ${sid(s)}${s.machine ? ` · pushed by ${s.machine}` : ''}`,
  keywords: [s.id, s.project, s.machine, s.agent].filter(Boolean),
})))

const enabled = computed(() => props.machines.filter((m) => !whyNotTarget(m)))
const machineOptions = computed(() => props.machines.map((m) => {
  const why = whyNotTarget(m)
  return { value: m.id, label: why ? `${m.name} (${why})` : m.name, disabled: !!why }
}))
// The machine that pushed the hub's current copy, which the hub insists on.
const fromOptions = computed(() => {
  const o = props.machines.map((m) => ({ value: m.id, label: m.name }))
  const n = session.value?.machine
  return n && !props.machines.some((m) => m.name === n) ? [...o, { value: n, label: `${n} (no longer joined)` }] : o
})
watch([op, picked, machine, from, exact], () => { error.value = '' })
watch(session, (s) => { from.value = (s && props.machines.find((m) => m.name === s.machine)?.id) || s?.machine || '' }, { immediate: true })

const opOptions = [{ value: 'push', label: 'Push from a machine' }, { value: 'pull', label: 'Pull to a machine' }]
const ready = computed(() => session.value && machine.value && (op.value === 'push' || from.value))

async function submit() {
  if (busy.value) return
  if (!ready.value) return void (error.value = op.value === 'push' ? 'Choose a session and the machine to push from.' : 'Choose a session, the machine to pull to, and the machine that pushed the copy.')
  const s = session.value
  const body = { op: op.value, machine: machine.value, agent: s.agent, session: s.id }
  if (op.value === 'push') body.exact = exact.value
  else body.from = from.value
  busy.value = true
  error.value = ''
  try {
    emit('created', await props.api.createCommand(body))
  } catch (e) {
    if (e instanceof AdminError && e.status === 401) return emit('expired')
    error.value = e.message
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <HDialog :open="true" title="New command" description="Ask a machine to push or pull one session. The machine runs it the next time it polls the hub." @close="emit('close')">
    <form class="dialog-form" @submit.prevent="submit">
      <HSegmentedControl v-model="op" label="Action" :options="opOptions" />
      <HCombobox v-model="picked" label="Session" placeholder="Search by title, id or project" :options="sessionOptions" empty-text="No stored session matches." :clearable="false" />
      <HAlert v-if="!enabled.length" tone="info" title="No machine can take commands"
        description="Run `asm control enable` on a machine and keep `asm daemon` running. It then shows up here once it has polled the hub." />
      <HSelect v-model="machine" :label="op === 'push' ? 'Machine to push from' : 'Machine to pull to'" placeholder="Choose a machine" :options="machineOptions" />
      <HSelect v-if="op === 'pull'" v-model="from" label="Copy pushed by" placeholder="Choose the machine that pushed it" :options="fromOptions"
        hint="The hub only hands out a copy that this machine pushed. It defaults to the machine that pushed the current copy." />
      <HCheckbox v-if="op === 'push'" v-model="exact" label="Exact push" description="Read the whole session and settle for nothing less than the hub holding exactly that copy." />
      <HAlert v-if="error" tone="danger" title="Not sent" :description="error" />
      <div class="dialog-actions">
        <HButton type="button" variant="ghost" label="Cancel" @click="emit('close')" />
        <HButton type="submit" variant="primary" :loading="busy" label="Send command" />
      </div>
    </form>
  </HDialog>
</template>
