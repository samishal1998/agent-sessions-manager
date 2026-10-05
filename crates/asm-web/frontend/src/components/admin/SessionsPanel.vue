<script setup>
import { computed, ref, watch } from 'vue'
import { HButton, HChipGroup, HDataTable, HEmptyState, HInput, HSelect, tableCellSlot } from '@hearth-ui/vue'
import AgentMark from '../AgentMark.vue'
import { ago, bytes } from '../../format.js'

// The stored sessions with search, filters and sort. The filters survive a
// refresh (sessionStorage); the token never goes near this.
const props = defineProps({ sessions: { type: Array, required: true }, sid: { type: Function, required: true } })
const emit = defineEmits(['open', 'remove', 'send'])

const KEY = 'asm-hub-admin-filters'
const blank = { q: '', agents: [], machine: '', sort: 'newest' }
const load = () => {
  try {
    return { ...blank, ...JSON.parse(sessionStorage.getItem(KEY) || '{}') }
  } catch {
    return { ...blank }
  }
}
const f = ref(load())
watch(f, (v) => {
  try {
    sessionStorage.setItem(KEY, JSON.stringify(v))
  } catch {
    /* private window: filters last as long as the page */
  }
}, { deep: true })

const sorts = [
  { value: 'newest', label: 'Newest pushed' }, { value: 'oldest', label: 'Oldest pushed' }, { value: 'largest', label: 'Largest' },
  { value: 'revisions', label: 'Most revisions' }, { value: 'title', label: 'Title A to Z' },
]
const agentOptions = computed(() => [...new Set(props.sessions.map((s) => s.agent))].sort().map((a) => ({ value: a, label: a })))
const machineOptions = computed(() => [{ value: '', label: 'All machines' }, ...[...new Set(props.sessions.map((s) => s.machine).filter(Boolean))].sort().map((m) => ({ value: m, label: m }))])
const name = (s) => s.title || 'Untitled session'
const proj = (s) => (s.project || '').split('/').filter(Boolean).pop() || '—'
const cmp = {
  newest: (a, b) => (b.pushed_at || '').localeCompare(a.pushed_at || ''),
  oldest: (a, b) => (a.pushed_at || '').localeCompare(b.pushed_at || ''),
  largest: (a, b) => b.size - a.size,
  revisions: (a, b) => b.revisions - a.revisions,
  title: (a, b) => name(a).localeCompare(name(b)),
}
const shown = computed(() => {
  const q = f.value.q.trim().toLowerCase()
  return props.sessions
    .filter((s) => (!f.value.agents.length || f.value.agents.includes(s.agent)) && (!f.value.machine || s.machine === f.value.machine)
      && (!q || [s.title, s.id, s.project, s.machine, s.agent].some((x) => x && String(x).toLowerCase().includes(q))))
    .sort(cmp[f.value.sort] || cmp.newest)
})
const filtered = computed(() => f.value.q.trim() || f.value.agents.length || f.value.machine)
const clear = () => { f.value = { ...blank, sort: f.value.sort } }

const revs = (s) => `${s.revisions} ${s.revisions === 1 ? 'revision' : 'revisions'}`
const key = (s) => `${s.agent}/${s.id}`
const byKey = computed(() => Object.fromEntries(props.sessions.map((s) => [key(s), s])))
const cols = [
  { key: 'title', label: 'Session' }, { key: 'project', label: 'Project' }, { key: 'machine', label: 'From' },
  { key: 'size', label: 'Size' }, { key: 'pushed', label: 'Pushed' }, { key: 'actions', label: 'Actions' },
]
const rows = computed(() => shown.value.map((s) => ({ id: key(s), title: name(s), project: proj(s), machine: s.machine || '—', size: bytes(s.size), pushed: ago(s.pushed_at), actions: '' })))
</script>

<template>
  <div class="admin-stack">
    <div class="sp-filters">
      <HInput v-model="f.q" class="sp-search" type="search" label="Search sessions" placeholder="Title, id, project, machine, agent" />
      <HSelect v-model="f.machine" label="Machine" :options="machineOptions" />
      <HSelect v-model="f.sort" label="Sort by" :options="sorts" />
    </div>
    <HChipGroup v-if="agentOptions.length > 1" v-model="f.agents" label="Agent" :options="agentOptions" />
    <p class="sp-count" role="status">
      {{ shown.length }} of {{ sessions.length }} sessions
      <HButton v-if="filtered && shown.length" size="compact" variant="ghost" label="Clear filters" @click="clear" />
    </p>

    <HEmptyState v-if="!shown.length" icon="search" :title="sessions.length ? 'No session matches' : 'Nothing has been pushed yet'"
      :description="sessions.length ? 'Loosen the search or remove a filter.' : 'Run asm push on a joined machine and its sessions appear here.'">
      <HButton v-if="sessions.length" variant="primary" label="Clear filters" @click="clear" />
    </HEmptyState>

    <ul v-else class="admin-cards" aria-label="Sessions stored on this hub">
      <li v-for="s in shown" :key="key(s)" class="admin-card">
        <span class="admin-session"><AgentMark :agent="s.agent" :size="16" /><button type="button" class="admin-link" :title="name(s)" @click="emit('open', s)">{{ name(s) }}</button></span>
        <span class="admin-meta"><span class="admin-mono">{{ sid(s) }}</span> · <span class="admin-clip" :title="s.project">{{ proj(s) }}</span> · from {{ s.machine || '—' }}</span>
        <span class="admin-meta">{{ bytes(s.size) }} · {{ revs(s) }} · {{ ago(s.pushed_at) }}</span>
        <span class="admin-actions">
          <HButton size="compact" @click="emit('open', s)">View transcript<span class="admin-sr"> of {{ name(s) }}</span></HButton>
          <HButton size="compact" @click="emit('send', s)">Send to machine…<span class="admin-sr"> {{ name(s) }}</span></HButton>
          <HButton size="compact" variant="danger" @click="emit('remove', s)">Delete<span class="admin-sr"> {{ name(s) }}</span></HButton>
        </span>
      </li>
    </ul>
    <HDataTable v-if="shown.length" class="admin-table" label="Sessions stored on this hub" :rows="rows" :columns="cols">
      <template v-for="r in rows" :key="r.id" #[tableCellSlot(r.id,'title')]>
        <span class="admin-session">
          <AgentMark :agent="byKey[r.id].agent" :size="16" />
          <span class="sp-two"><button type="button" class="admin-link" :title="r.title" @click="emit('open', byKey[r.id])">{{ r.title }}</button><span class="admin-meta"><span class="admin-mono">{{ sid(byKey[r.id]) }}</span> · {{ revs(byKey[r.id]) }}</span></span>
        </span>
      </template>
      <template v-for="r in rows" :key="r.id + 'p'" #[tableCellSlot(r.id,'project')]><span class="admin-clip sp-cell" :title="byKey[r.id].project">{{ r.project }}</span></template>
      <template v-for="r in rows" :key="r.id + 'm'" #[tableCellSlot(r.id,'machine')]><span class="admin-clip sp-cell" :title="r.machine">{{ r.machine }}</span></template>
      <template v-for="r in rows" :key="r.id + 'a'" #[tableCellSlot(r.id,'actions')]>
        <span class="admin-actions">
          <HButton size="compact" @click="emit('open', byKey[r.id])">View transcript<span class="admin-sr"> of {{ r.title }}</span></HButton>
          <HButton size="compact" @click="emit('send', byKey[r.id])">Send to machine…<span class="admin-sr"> {{ r.title }}</span></HButton>
          <HButton size="compact" variant="danger" @click="emit('remove', byKey[r.id])">Delete<span class="admin-sr"> {{ r.title }}</span></HButton>
        </span>
      </template>
    </HDataTable>
  </div>
</template>

<style>
.sp-filters { display: grid; grid-template-columns: minmax(0, 2fr) minmax(0, 1fr) minmax(0, 1fr); gap: 16px; align-items: end; }
.sp-count { margin: 0; display: flex; align-items: center; gap: 8px; min-height: 32px; font-size: 14px; color: var(--h-muted); }
@media (max-width: 640px) { .sp-filters { grid-template-columns: 1fr; gap: 12px; } }
.sp-cell { display: block; max-width: 16ch; }
.sp-two { display: grid; min-width: 0; }
.admin-table .admin-session { max-width: 26rem; }
</style>
