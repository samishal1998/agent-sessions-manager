<script setup>
import { computed, ref, watch } from 'vue'
import { HAlert, HButton, HCheckbox, HCombobox, HDialog, HSegmentedControl, HSelect } from '@hearth-ui/vue'
import { AdminError } from '../../admin-api.js'
import { whyNotSource, whyNotTarget } from '../../commands.js'
import { shortId } from '../../ids.js'

// Ask one machine to push or pull one session, send a copy from one machine to another (a plan: push, then pull), or move it (the same, then
// archive it on the first). A move needs an explicit confirmation. Mounted only while open, so every opening starts from a clean form.
const props = defineProps({
  sessions: { type: Array, required: true },
  machines: { type: Array, required: true },
  api: { type: Object, required: true },
  prefill: { type: Object, default: null }, // a hub session: from "Send to machine…" or "Move to machine…"
  action: { type: String, default: 'send' }, // with a prefill: which of the two
})
const emit = defineEmits(['close', 'created', 'expired'])

const key = (s) => `${s.agent}/${s.id}`
const op = ref(props.prefill ? props.action : 'push')
const picked = ref(props.prefill ? key(props.prefill) : '')
const machine = ref('')
const to = ref('')
const sendFrom = ref('') // send: the machine to push from (a pull's `from` is the copy's pusher)
const from = ref('')
const exact = ref(false)
const confirm = ref(false) // move: the owner agrees to the archive on the source
const error = ref('')
const busy = ref(false)

const nameOf = (id) => props.machines.find((m) => m.id === id)?.name
const confirmLabel = computed(() => `Archive on ${nameOf(sendFrom.value) || 'the first machine'} after ${nameOf(to.value) || 'the second'} has it. Nothing is deleted; asm unarchive brings it back.`)
const move = computed(() => op.value === 'move')
const twoEnds = computed(() => op.value === 'send' || move.value)
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
// The option keeps the reason short (up to its colon); the full advice sits under the field.
const optionFor = (m, why, skip) => ({ value: m.id, label: why ? `${m.name} (${why.split(':')[0]})` : m.name, disabled: !!why || m.id === skip })
const optionsFor = (o, skip = '') => props.machines.map((m) => optionFor(m, whyNotTarget(m, o), skip))
const machineOptions = computed(() => optionsFor(op.value))
// Send: the two ends must differ, so each list greys out the machine picked in the other.
const pushOptions = computed(() => (move.value ? props.machines.map((m) => optionFor(m, whyNotSource(m), to.value)) : optionsFor('push', to.value)))
// A move needs archive on the source, which its owner turns on separately: say so under the field whenever a machine is greyed out for it.
const archiveOff = computed(() => move.value && props.machines.some((m) => /^archive is off|^needs a newer/.test(whyNotSource(m))))
const pushHint = computed(() => (move.value ? `The machine that holds the session. It archives it once the other machine has it, so it must allow push and archive.${archiveOff.value ? ' A machine marked "archive is off" needs asm control enable --allow push,pull,archive run on it; one that "needs a newer asm" must be upgraded.' : ''}` : 'The machine that holds the copy to send. It keeps it.'))
const pullOptions = computed(() => optionsFor('pull', sendFrom.value))
// The machine that pushed the hub's current copy, which the hub insists on.
const fromOptions = computed(() => {
  const o = props.machines.map((m) => ({ value: m.id, label: m.name }))
  const n = session.value?.machine
  return n && !props.machines.some((m) => m.name === n) ? [...o, { value: n, label: `${n} (no longer joined)` }] : o
})
watch([op, picked, machine, to, sendFrom, from, exact, confirm], () => { error.value = '' })
// The tick names the machine that gets archived, so it must not outlive a change of that machine or of the action.
watch([op, sendFrom, to, picked], () => { confirm.value = false })
watch(session, (s) => {
  const joined = s && props.machines.find((m) => m.name === s.machine)?.id
  from.value = joined || s?.machine || ''
  sendFrom.value = joined || '' // a pusher that left cannot push again
}, { immediate: true })

const opOptions = [{ value: 'push', label: 'Push' }, { value: 'pull', label: 'Pull' }, { value: 'send', label: 'Send to another machine' }, { value: 'move', label: 'Move to another machine' }]
const ready = computed(() => session.value && (twoEnds.value ? sendFrom.value && to.value && sendFrom.value !== to.value : machine.value && (op.value === 'push' || from.value)))
const SAY = { push: 'Choose a session and the machine to push from.', pull: 'Choose a session, the machine to pull to, and the machine that pushed the copy.', send: 'Choose a session and two different machines: one to push from, one to pull to.', move: 'Choose a session and two different machines: one to move it from, one to move it to.' }

async function submit() {
  if (busy.value) return
  if (!ready.value) return void (error.value = SAY[op.value])
  const s = session.value
  if (move.value && !confirm.value) return
  const send = twoEnds.value
  const body = move.value ? { kind: 'move', agent: s.agent, session: s.id, from: sendFrom.value, to: to.value, exact: true, confirm_archive: sendFrom.value }
    : send ? { kind: 'send', agent: s.agent, session: s.id, from: sendFrom.value, to: to.value, exact: exact.value } : { op: op.value, machine: machine.value, agent: s.agent, session: s.id }
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
  <HDialog :open="true" :description="move ? 'Move a session from one machine to another: the first pushes exactly what it has to the hub, the second pulls that copy, and only then the first archives its own. Nothing is deleted; asm unarchive brings it back. Only sessions already on the hub are listed.' : op === 'send' ? 'Copy a session from one machine to another: the first pushes it to the hub, then the second pulls exactly that copy. The first machine keeps its copy; nothing is archived or deleted. Only sessions already on the hub are listed.' : 'Ask a machine to push or pull one session already on the hub. The machine runs it the next time it polls the hub.'" title="New command" @close="emit('close')">
    <form class="dialog-form" @submit.prevent="submit">
      <HSegmentedControl v-model="op" label="Action" :options="opOptions" />
      <HCombobox v-model="picked" label="Session" placeholder="Search by title, id or project" :options="sessionOptions" empty-text="No session on the hub matches." :clearable="false" />
      <HAlert v-if="!enabled.length" tone="info" title="No machine can take commands"
        description="Run `asm control enable` on a machine and keep `asm daemon` running. It then shows up here once it has polled the hub." />
      <template v-if="twoEnds">
        <HSelect v-model="sendFrom" label="Push from" placeholder="Choose a machine" :options="pushOptions" :hint="pushHint" />
        <HSelect v-model="to" label="Pull to" placeholder="Choose a machine" :options="pullOptions" hint="It installs a copy of what the first machine pushed, and only after that push succeeded." />
      </template>
      <HSelect v-else v-model="machine" :label="op === 'push' ? 'Machine to push from' : 'Machine to pull to'" placeholder="Choose a machine" :options="machineOptions" />
      <HSelect v-if="op === 'pull'" v-model="from" label="Copy pushed by" placeholder="Choose the machine that pushed it" :options="fromOptions"
        hint="The hub only hands out a copy that this machine pushed. It defaults to the machine that pushed the current copy." />
      <HCheckbox v-if="op === 'push' || op === 'send'" v-model="exact" label="Exact push" description="Read the whole session and settle for nothing less than the hub holding exactly that copy." />
      <HCheckbox v-if="move" v-model="confirm" required :label="confirmLabel" description="Required. The session moves only if the first machine still has exactly the copy that was sent when it archives." />
      <HAlert v-if="error" tone="danger" title="Not sent" :description="error" />
      <p v-if="move && !confirm" id="nc-tick" class="nc-tick">Tick the box to enable Move</p>
      <div class="dialog-actions">
        <HButton type="button" variant="ghost" label="Cancel" @click="emit('close')" />
        <HButton type="submit" variant="primary" :loading="busy" :disabled="move && !confirm" :label="move ? 'Move the session' : op === 'send' ? 'Send a copy' : 'Send command'" />
      </div>
    </form>
  </HDialog>
</template>

<style>
.nc-tick { margin: 0; font-size: 14px; color: var(--h-muted); text-align: right; }
</style>
