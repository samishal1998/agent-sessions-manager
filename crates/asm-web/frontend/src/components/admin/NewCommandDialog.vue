<script setup>
import { computed, ref, watch } from 'vue'
import { HAlert, HButton, HCheckbox, HCombobox, HDialog, HSegmentedControl, HSelect } from '@hearth-ui/vue'
import { AdminError } from '../../admin-api.js'
import { whyNotTarget } from '../../commands.js'
import { shortId } from '../../ids.js'

// Ask one machine to push or pull one session, or send it from one machine to another (a plan: push, then pull). Mounted only while open, so
// every opening starts from a clean form.
const props = defineProps({
  sessions: { type: Array, required: true },
  machines: { type: Array, required: true },
  api: { type: Object, required: true },
  prefill: { type: Object, default: null }, // a hub session: from "Send to machine…"
})
const emit = defineEmits(['close', 'created', 'expired'])

const key = (s) => `${s.agent}/${s.id}`
const op = ref(props.prefill ? 'send' : 'push')
const picked = ref(props.prefill ? key(props.prefill) : '')
const machine = ref('')
const to = ref('')
const sendFrom = ref('') // send: the machine to push from (a pull's `from` is the copy's pusher)
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
// A machine can be chosen for an op only when remote control is on there and it allows that op.
const optionsFor = (o, skip = '') => props.machines.map((m) => {
  const why = whyNotTarget(m, o)
  return { value: m.id, label: why ? `${m.name} (${why})` : m.name, disabled: !!why || m.id === skip }
})
const machineOptions = computed(() => optionsFor(op.value))
// Send: the two ends must differ, so each list greys out the machine picked in the other.
const pushOptions = computed(() => optionsFor('push', to.value))
const pullOptions = computed(() => optionsFor('pull', sendFrom.value))
// The machine that pushed the hub's current copy, which the hub insists on.
const fromOptions = computed(() => {
  const o = props.machines.map((m) => ({ value: m.id, label: m.name }))
  const n = session.value?.machine
  return n && !props.machines.some((m) => m.name === n) ? [...o, { value: n, label: `${n} (no longer joined)` }] : o
})
watch([op, picked, machine, to, sendFrom, from, exact], () => { error.value = '' })
watch(session, (s) => {
  const joined = s && props.machines.find((m) => m.name === s.machine)?.id
  from.value = joined || s?.machine || ''
  sendFrom.value = joined || '' // a pusher that left cannot push again
}, { immediate: true })

const opOptions = [{ value: 'push', label: 'Push' }, { value: 'pull', label: 'Pull' }, { value: 'send', label: 'Send to another machine' }]
const ready = computed(() => session.value && (op.value === 'send' ? sendFrom.value && to.value && sendFrom.value !== to.value : machine.value && (op.value === 'push' || from.value)))
const SAY = { push: 'Choose a session and the machine to push from.', pull: 'Choose a session, the machine to pull to, and the machine that pushed the copy.', send: 'Choose a session and two different machines: one to push from, one to pull to.' }

async function submit() {
  if (busy.value) return
  if (!ready.value) return void (error.value = SAY[op.value])
  const s = session.value
  const send = op.value === 'send'
  const body = send ? { kind: 'send', agent: s.agent, session: s.id, from: sendFrom.value, to: to.value, exact: exact.value } : { op: op.value, machine: machine.value, agent: s.agent, session: s.id }
  if (op.value === 'push') body.exact = exact.value
  else if (op.value === 'pull') body.from = from.value
  busy.value = true
  error.value = ''
  try {
    emit('created', await (send ? props.api.createPlan(body) : props.api.createCommand(body)))
  } catch (e) {
    if (e instanceof AdminError && e.status === 401) return emit('expired')
    error.value = e.message
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <HDialog :open="true" :description="op === 'send' ? 'Copy a session from one machine to another: the first pushes it to the hub, then the second pulls exactly that copy. The first machine keeps its copy; nothing is archived or deleted. Only sessions already on the hub are listed.' : 'Ask a machine to push or pull one session already on the hub. The machine runs it the next time it polls the hub.'" title="New command" @close="emit('close')">
    <form class="dialog-form" @submit.prevent="submit">
      <HSegmentedControl v-model="op" label="Action" :options="opOptions" />
      <HCombobox v-model="picked" label="Session" placeholder="Search by title, id or project" :options="sessionOptions" empty-text="No session on the hub matches." :clearable="false" />
      <HAlert v-if="!enabled.length" tone="info" title="No machine can take commands"
        description="Run `asm control enable` on a machine and keep `asm daemon` running. It then shows up here once it has polled the hub." />
      <template v-if="op === 'send'">
        <HSelect v-model="sendFrom" label="Push from" placeholder="Choose a machine" :options="pushOptions" :hint="sendFrom ? '' : 'The machine that holds the copy to send. It keeps it.'" />
        <HSelect v-model="to" label="Pull to" placeholder="Choose a machine" :options="pullOptions" hint="It installs a copy of what the first machine pushed, and only after that push succeeded." />
      </template>
      <HSelect v-else v-model="machine" :label="op === 'push' ? 'Machine to push from' : 'Machine to pull to'" placeholder="Choose a machine" :options="machineOptions" />
      <HSelect v-if="op === 'pull'" v-model="from" label="Copy pushed by" placeholder="Choose the machine that pushed it" :options="fromOptions"
        hint="The hub only hands out a copy that this machine pushed. It defaults to the machine that pushed the current copy." />
      <HCheckbox v-if="op !== 'pull'" v-model="exact" label="Exact push" description="Read the whole session and settle for nothing less than the hub holding exactly that copy." />
      <HAlert v-if="error" tone="danger" title="Not sent" :description="error" />
      <div class="dialog-actions">
        <HButton type="button" variant="ghost" label="Cancel" @click="emit('close')" />
        <HButton type="submit" variant="primary" :loading="busy" :label="op === 'send' ? 'Send a copy' : 'Send command'" />
      </div>
    </form>
  </HDialog>
</template>
