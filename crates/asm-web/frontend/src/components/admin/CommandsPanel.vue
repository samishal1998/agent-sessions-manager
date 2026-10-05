<script setup>
import { computed, ref } from 'vue'
import { HAlert, HButton, HChipGroup, HDataTable, HEmptyState, HInput, HSelect, tableCellSlot } from '@hearth-ui/vue'
import { Ban, Check, Clock, Hourglass, LoaderCircle, TriangleAlert } from 'lucide-vue-next'
import AgentMark from '../AgentMark.vue'
import StatusBadge from './StatusBadge.vue'
import { AdminError } from '../../admin-api.js'
import { STATES, STATE_LABEL, STATE_TONE, canRetry, isOpen, moreDetail, resultText } from '../../commands.js'
import { confirmDialog } from '../../dialogs.js'
import { ago } from '../../format.js'
import { shortId } from '../../ids.js'
import { notify } from '../../toasts.js'

// The hub's command queue: what was asked of each machine and how it went.
// Filters live here; the list itself is owned (and auto-refreshed) by the page.
const props = defineProps({
  commands: { type: Array, required: true },
  machines: { type: Array, required: true },
  api: { type: Object, required: true },
  error: { type: String, default: '' },
})
const emit = defineEmits(['new', 'changed', 'expired'])

const ICONS = { queued: Clock, running: LoaderCircle, ok: Check, blocked: TriangleAlert, cancelled: Ban, expired: Hourglass }
const blank = { q: '', states: [], machine: '' }
const f = ref({ ...blank })
const busy = ref('')

const count = (s) => props.commands.filter((c) => c.state === s).length
const stateOptions = computed(() => STATES.map((s) => ({ value: s, label: `${STATE_LABEL[s]} (${count(s)})` })))
const machineOptions = computed(() => {
  const seen = new Map(props.machines.map((m) => [m.id, m.name]))
  for (const c of props.commands) for (const m of [c.machine, c.from]) if (m && !seen.has(m.id)) seen.set(m.id, m.name)
  return [{ value: '', label: 'All machines' }, ...[...seen].map(([value, label]) => ({ value, label })).sort((a, b) => a.label.localeCompare(b.label))]
})
const name = (c) => c.title || 'Untitled session'
const sid = (c) => shortId({ ref: { agent: c.agent, native_id: c.session } })
const shown = computed(() => {
  const q = f.value.q.trim().toLowerCase()
  return props.commands.filter((c) => (!f.value.states.length || f.value.states.includes(c.state))
    && (!f.value.machine || c.machine.id === f.value.machine || c.from?.id === f.value.machine)
    && (!q || [c.title, c.session, c.id, c.agent, c.op, c.machine.name, c.from?.name, c.code, c.detail, STATE_LABEL[c.state]].some((x) => x && String(x).toLowerCase().includes(q))))
})
const filtered = computed(() => f.value.q.trim() || f.value.states.length || f.value.machine)
const clear = () => { f.value = { ...blank } }
const byId = computed(() => Object.fromEntries(props.commands.map((c) => [c.id, c])))

const verb = (c) => (c.op === 'push' ? 'Push' : 'Pull')
const where = (c) => (c.op === 'push' ? `from ${c.machine.name}` : `to ${c.machine.name}`)
const extra = (c) => [c.op === 'pull' && c.from ? `copy pushed by ${c.from.name}` : '', c.rev ? `pinned to ${c.rev.slice(0, 8)}` : '', c.args?.exact ? 'exact' : ''].filter(Boolean).join(' · ')
const tries = (c) => `${c.attempts} ${c.attempts === 1 ? 'attempt' : 'attempts'}`
const full = (t) => (t ? new Date(t).toLocaleString() : '')
const what = (c) => `${verb(c).toLowerCase()} of ${name(c)}`

async function act(c, fn, done) {
  busy.value = c.id
  try {
    await fn()
    notify(done)
    emit('changed')
  } catch (e) {
    if (e instanceof AdminError && e.status === 401) return emit('expired')
    notify('That did not work', { tone: 'danger', description: e.message })
    emit('changed')
  } finally {
    busy.value = ''
  }
}
async function cancel(c) {
  const running = c.state === 'running'
  const ok = await confirmDialog({
    title: `Cancel this ${c.op}?`,
    description: running
      ? `${c.machine.name} has already started it, so it may still finish.`
      : `${c.machine.name} has not picked it up yet. It will not run.`,
    confirmLabel: 'Cancel command',
    danger: true,
  })
  if (ok) act(c, () => props.api.cancelCommand(c.id), running ? 'Cancel requested. It may still finish.' : 'Command cancelled.')
}
const retry = (c) => act(c, () => props.api.retryCommand(c.id), `Queued ${what(c)} again.`)

const cols = [
  { key: 'state', label: 'Status' }, { key: 'what', label: 'Command' }, { key: 'machine', label: 'Machine' },
  { key: 'when', label: 'Created' }, { key: 'actions', label: 'Actions' },
]
const rows = computed(() => shown.value.map((c) => ({ id: c.id, state: STATE_LABEL[c.state], what: name(c), machine: c.machine.name, when: ago(c.created), actions: '' })))
</script>

<template>
  <div class="admin-stack">
    <HAlert v-if="error" tone="warning" title="Could not load commands" :description="error" />

    <HEmptyState v-if="!commands.length && !error" icon="server" title="No remote commands yet"
      description="Remote control lets this hub ask a machine to push or pull one session. On a machine, run `asm control enable`, and keep `asm daemon` running so it can poll for commands. Then send the first one from here or from the CLI.">
      <HButton variant="primary" label="New command" @click="emit('new')" />
    </HEmptyState>

    <template v-else-if="commands.length">
      <div class="cp-head">
        <div class="sp-filters cp-filters">
          <HInput v-model="f.q" class="sp-search" type="search" label="Search commands" placeholder="Session, machine, result" />
          <HSelect v-model="f.machine" label="Machine" :options="machineOptions" />
        </div>
        <HButton variant="primary" label="New command" @click="emit('new')" />
      </div>
      <HChipGroup v-model="f.states" label="State" :options="stateOptions" />
      <p class="sp-count" role="status">
        {{ shown.length }} of {{ commands.length }} commands
        <HButton v-if="filtered && shown.length" size="compact" variant="ghost" label="Clear filters" @click="clear" />
      </p>

      <HEmptyState v-if="!shown.length" icon="search" title="No command matches" description="Loosen the search or remove a filter.">
        <HButton variant="primary" label="Clear filters" @click="clear" />
      </HEmptyState>

      <ul v-else class="admin-cards" aria-label="Remote commands">
        <li v-for="c in shown" :key="c.id" class="admin-card">
          <span class="admin-actions"><StatusBadge :tone="STATE_TONE[c.state]" :label="STATE_LABEL[c.state]" :icon="ICONS[c.state]" /><strong>{{ verb(c) }} {{ where(c) }}</strong></span>
          <span class="admin-session"><AgentMark :agent="c.agent" :size="16" /><span class="admin-clip" :title="name(c)">{{ name(c) }}</span></span>
          <span class="admin-meta"><span class="admin-mono">{{ sid(c) }}</span><template v-if="extra(c)"> · {{ extra(c) }}</template></span>
          <span class="cp-result">{{ resultText(c) }}</span>
          <span v-if="moreDetail(c)" class="admin-meta">{{ moreDetail(c) }}</span>
          <span class="admin-meta"><time :datetime="c.created" :title="full(c.created)">{{ ago(c.created) }}</time> · {{ tries(c) }}</span>
          <span class="admin-actions">
            <HButton v-if="isOpen(c) && !c.cancel_requested" size="compact" variant="danger" :loading="busy === c.id" @click="cancel(c)">Cancel<span class="admin-sr"> {{ what(c) }}</span></HButton>
            <HButton v-if="canRetry(c)" size="compact" :loading="busy === c.id" @click="retry(c)">Retry<span class="admin-sr"> {{ what(c) }}</span></HButton>
          </span>
        </li>
      </ul>

      <HDataTable v-if="shown.length" class="admin-table cp-table" label="Remote commands" :rows="rows" :columns="cols">
        <template v-for="r in rows" :key="r.id + 's'" #[tableCellSlot(r.id,'state')]>
          <span class="cp-status">
            <StatusBadge :tone="STATE_TONE[byId[r.id].state]" :label="STATE_LABEL[byId[r.id].state]" :icon="ICONS[byId[r.id].state]" />
            <span class="cp-result">{{ resultText(byId[r.id]) }}</span>
            <span v-if="moreDetail(byId[r.id])" class="admin-meta">{{ moreDetail(byId[r.id]) }}</span>
          </span>
        </template>
        <template v-for="r in rows" :key="r.id + 'w'" #[tableCellSlot(r.id,'what')]>
          <span class="admin-session">
            <AgentMark :agent="byId[r.id].agent" :size="16" />
            <span class="sp-two"><span class="admin-clip" :title="r.what"><strong>{{ verb(byId[r.id]) }}</strong> {{ r.what }}</span><span class="admin-meta"><span class="admin-mono">{{ sid(byId[r.id]) }}</span><template v-if="extra(byId[r.id]) && byId[r.id].op === 'push'"> · {{ extra(byId[r.id]) }}</template></span></span>
          </span>
        </template>
        <template v-for="r in rows" :key="r.id + 'm'" #[tableCellSlot(r.id,'machine')]>
          <span class="sp-two"><span class="admin-clip sp-cell" :title="r.machine">{{ where(byId[r.id]) }}</span><span v-if="byId[r.id].op === 'pull' && extra(byId[r.id])" class="admin-meta">{{ extra(byId[r.id]) }}</span></span>
        </template>
        <template v-for="r in rows" :key="r.id + 't'" #[tableCellSlot(r.id,'when')]>
          <span class="sp-two"><time :datetime="byId[r.id].created" :title="full(byId[r.id].created)">{{ r.when }}</time><span class="admin-meta">{{ tries(byId[r.id]) }}</span></span>
        </template>
        <template v-for="r in rows" :key="r.id + 'a'" #[tableCellSlot(r.id,'actions')]>
          <span class="admin-actions">
            <HButton v-if="isOpen(byId[r.id]) && !byId[r.id].cancel_requested" size="compact" variant="danger" :loading="busy === r.id" @click="cancel(byId[r.id])">Cancel<span class="admin-sr"> {{ what(byId[r.id]) }}</span></HButton>
            <HButton v-if="canRetry(byId[r.id])" size="compact" :loading="busy === r.id" @click="retry(byId[r.id])">Retry<span class="admin-sr"> {{ what(byId[r.id]) }}</span></HButton>
          </span>
        </template>
      </HDataTable>
    </template>
  </div>
</template>

<style>
.cp-head { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 16px; align-items: end; }
.cp-filters { grid-template-columns: minmax(0, 2fr) minmax(0, 1fr); }
.cp-status { display: grid; gap: 4px; justify-items: start; white-space: normal; max-width: 34ch; }
.cp-result { white-space: normal; overflow-wrap: anywhere; font-size: 14px; }
.cp-table td { vertical-align: top; }
.cp-table .sp-cell { max-width: 22ch; }
.cp-table .admin-session { max-width: 24rem; }
@media (max-width: 640px) { .cp-head, .cp-filters { grid-template-columns: 1fr; } }
</style>
