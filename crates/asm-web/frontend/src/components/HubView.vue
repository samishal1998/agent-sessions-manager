<script setup>
import {
  HAlert, HBadge, HButton, HCard, HChipGroup, HConnectionState, HCopyField, HDataTable,
  HDescriptionList, HEmptyState, HInput, HList, HListItem, HPageHeader, HSheet, HStatCard,
  HSwitch, HTimeline, tableCellSlot,
} from '@hearth-ui/vue'
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { ArrowDown, ArrowUp, ArrowUpDown, Check, Monitor, RefreshCw, Scale } from 'lucide-vue-next'
import AgentMark from './AgentMark.vue'
import { ago, agoUnix, bytes, hubTone } from '../format.js'
import { shortProject } from '../ids.js'
import '../styles/hubview.css'

// Everything about how this machine stands with the hub: the connection, the
// daemon, the other machines, and every session here and on the hub with what
// each needs and why. State wording and next steps come from the core (`RowState`).
const props = defineProps({
  hub: { type: Object, default: null },
  checking: { type: Boolean, default: false },
  checkedAt: { type: Number, default: 0 },
  home: { type: String, default: null },
})
const emit = defineEmits(['check', 'push', 'pull', 'compare', 'push-needed', 'pull-all'])

const now = ref(Date.now())
let clock = null
onMounted(() => (clock = setInterval(() => (now.value = Date.now()), 15000)))
onUnmounted(() => clearInterval(clock))

const fresh = computed(() => props.hub?.connected && !props.hub?.stale)
const host = computed(() => {
  try {
    return new URL(props.hub.url).host
  } catch {
    return props.hub?.url || 'the hub'
  }
})
const summary = computed(() => props.hub?.summary || {})
const daemon = computed(() => props.hub?.daemon || null)
const machines = computed(() => props.hub?.machines || [])
const rows = computed(() => props.hub?.rows || [])
const when = (ts) => ago(ts, now.value)
// The hub's portable form (`${HOME}/…`) reads as `~/…`, like local paths.
const dir = (p) => shortProject((p || '').replace('${HOME}', props.home || '~'), props.home)

/* Connection / daemon ------------------------------------------------- */
const connState = computed(() => (props.checking ? 'connecting' : fresh.value ? 'connected' : 'failed'))
const connLabel = computed(() => (props.checking ? 'Checking the hub' : fresh.value ? 'Hub reachable' : "Can't reach the hub"))
const connItems = computed(() => [
  { key: 'hub', label: 'Hub', value: host.value },
  { key: 'me', label: 'This machine', value: props.hub?.machine },
  { key: 'at', label: fresh.value ? 'Checked' : 'Last reached', value: props.checkedAt ? when(props.checkedAt) : 'not yet' },
])
const daemonHealthy = computed(() => daemon.value?.state === 'running' && !daemon.value.file?.last_error)
const daemonLabel = computed(() =>
  ({ running: 'Running', hung: 'Not responding' })[daemon.value?.state] || 'Not running')
const daemonTone = computed(() => (daemonHealthy.value ? 'success' : daemon.value?.state === 'not_running' ? 'neutral' : 'danger'))
const daemonItems = computed(() => {
  const f = daemon.value?.file
  if (!f) return []
  return [
    { key: 'push', label: 'Last push', value: agoUnix(f.last_push, now.value) || 'none yet' },
    { key: 'pass', label: 'Last pass', value: agoUnix(f.last_pass, now.value) || 'none yet' },
    { key: 'wait', label: 'Waiting', value: `${f.pending}${f.idle ? ` · ${f.idle} idle` : ''}` },
    {
      key: 'what', label: 'Pushes',
      value: f.active_window ? `live or changed in the last ${Math.round(f.active_window / 60)} min` : 'everything that differs',
    },
    { key: 'every', label: 'Every', value: `${f.interval}s` },
  ]
})
const recent = computed(() =>
  (daemon.value?.file?.recent || []).slice().reverse().slice(0, 8).map((e, i) => ({
    id: String(i), title: e.line, timestamp: agoUnix(e.at, now.value), tone: e.ok ? 'neutral' : 'danger',
  })),
)
const showAllMachines = ref(false)
const shownMachines = computed(() => (showAllMachines.value ? machines.value : machines.value.slice(0, 5)))

/* Filters --------------------------------------------------------------- */
const ORDER = ['diverged', 'untracked', 'behind', 'remote', 'ahead', 'local', 'in_sync']
const PRIORITY = Object.fromEntries(ORDER.map((s, i) => [s, i]))
const query = ref('')
const picked = ref([])
const onlyAttention = ref(false)
const page = ref(1)

// Chips use the rows' own labels so chip and badge never disagree (a compared
// 'untracked' row reads 'Differs'); the label is the filter key.
const stateOptions = computed(() => {
  const g = new Map()
  for (const r of rows.value) {
    const e = g.get(r.label) || { n: 0, p: PRIORITY[r.state] }
    e.n++
    e.p = Math.min(e.p, PRIORITY[r.state])
    g.set(r.label, e)
  }
  return [...g].sort((a, b) => a[1].p - b[1].p).map(([label, e]) => ({ value: label, label: `${label} (${e.n})` }))
})
const shown = computed(() => {
  const q = query.value.trim().toLowerCase()
  return rows.value
    .filter((r) => !picked.value.length || picked.value.includes(r.label))
    .filter((r) => !onlyAttention.value || r.action)
    .filter(
      (r) =>
        !q ||
        [r.title, r.id, r.short_id, r.project, r.project_root, r.machine, r.agent, r.branch]
          .filter(Boolean)
          .some((x) => String(x).toLowerCase().includes(q)),
    )
    .slice()
    .sort(
      (a, b) =>
        PRIORITY[a.state] - PRIORITY[b.state] ||
        a.project.localeCompare(b.project) ||
        String(b.updated).localeCompare(String(a.updated)),
    )
})
watch([query, picked, onlyAttention], () => (page.value = 1))

/* Table ------------------------------------------------------------------ */
const rowKey = (r) => `${r.agent}:${r.id}`
const byKey = computed(() => new Map(shown.value.map((r) => [rowKey(r), r])))
const ALL = [
  { key: 'sync', label: 'Sync' },
  { key: 'session', label: 'Session' },
  { key: 'project', label: 'Project' },
  { key: 'where', label: 'Where' },
  { key: 'updated', label: 'Updated' },
  { key: 'act', label: 'Actions', align: 'end' },
]
const columns = computed(() => (compact.value ? ALL.filter((c) => c.key !== 'where' && c.key !== 'updated') : ALL))
const where = (r) => (r.here && r.on_hub ? 'Both' : r.on_hub ? 'Hub only' : 'This machine only')
const tableRows = computed(() =>
  shown.value.map((r) => ({
    id: rowKey(r), sync: r.label, session: r.title || 'Untitled session',
    project: dir(r.project), where: where(r), updated: when(r.updated), act: '',
  })),
)
const cell = (r, key) => tableCellSlot(rowKey(r), key)
const STATE_ICON = { in_sync: Check, diverged: ArrowUpDown, ahead: ArrowUp, local: ArrowUp }
const iconOf = (r) => STATE_ICON[r.state] || ArrowDown

// What the row's button does, or why it cannot.
function buttonOf(r) {
  if (!r.action) return null
  if (!fresh.value) return { text: r.action === 'push' ? 'Push' : 'Pull', off: 'The hub cannot be reached' }
  if (r.action === 'resolve') return { text: 'Resolve', off: null, resolve: true }
  if (r.action === 'pull' && !r.restorable)
    return { text: 'Pull', off: `${r.agent} sessions are backed up on the hub but cannot be restored here yet` }
  // Not yet looked at: compare first (nothing is installed). Once compared,
  // a difference is pulled to confirm which way it goes.
  if (r.state === 'untracked' && !r.compare) return { text: 'Compare', off: null, compare: true }
  return { text: r.action === 'push' ? 'Push' : 'Pull', off: null }
}

// HDataTable has no stacked layout of its own, so a phone gets a list.
const mq = window.matchMedia('(max-width: 640px)')
const mqc = window.matchMedia('(max-width: 1000px)')
const narrow = ref(mq.matches)
const compact = ref(mqc.matches) // drop Where/Updated so the action stays on screen
const onMq = (e) => (narrow.value = e.matches)
const onMqc = (e) => (compact.value = e.matches)
onMounted(() => { mq.addEventListener('change', onMq); mqc.addEventListener('change', onMqc) })
onUnmounted(() => { mq.removeEventListener('change', onMq); mqc.removeEventListener('change', onMqc) })

/* Details sheet ------------------------------------------------------------ */
const detailKey = ref(null)
const detail = computed(() => rows.value.find((r) => rowKey(r) === detailKey.value) || null)
const sheetOpen = computed(() => !!detail.value)
function press(r) {
  const b = buttonOf(r)
  if (b?.resolve) return (detailKey.value = rowKey(r))
  if (b?.compare) return emit('compare', r)
  emit(r.action === 'push' ? 'push' : 'pull', r)
}
// The command that does the same from a terminal.
function command(r) {
  const id = r.short_id || r.id
  return { resolve: `asm push ${id} --force`, push: `asm push ${id}`, pull: `asm pull ${id}` }[r.action] || ''
}
const hubCopy = (r) =>
  [`revision ${r.rev}`, bytes(r.size), r.pushed_at && `pushed ${when(r.pushed_at)}${r.machine ? ' by ' + r.machine : ''}`]
    .filter(Boolean).join(', ')
const detailItems = computed(() => {
  const r = detail.value
  if (!r) return []
  const items = [
    { key: 'id', label: 'Session id', value: r.id },
    {
      key: 'project', label: 'Project',
      value: dir(r.project_root) + (r.branch ? ` · ${r.branch}` : '') + (r.project !== r.project_root ? ` · as ${r.project}` : ''),
    },
    { key: 'where', label: 'Where it is', value: `${r.here ? 'on this machine' : 'not on this machine'}, ${r.on_hub ? 'on the hub' : 'not on the hub'}` },
  ]
  if (r.on_hub) items.push({ key: 'hub', label: 'Hub copy', value: hubCopy(r) })
  else if (r.size != null) items.push({ key: 'size', label: 'Size here', value: bytes(r.size) })
  if (r.agent_version) items.push({ key: 'ver', label: 'Agent version', value: r.agent_version })
  if (r.compare)
    items.push({
      key: 'cmp', label: 'Compared',
      value: `${bytes(r.compare.local_size)} here, ${bytes(r.compare.hub_size)} on the hub — they cover different files, so size does not say which is ahead; pulling does`,
    })
  items.push({ key: 'rest', label: 'Restorable here', value: r.restorable ? 'yes' : 'no — backed up only' })
  return items
})
</script>

<template>
  <section class="hubview" aria-label="Hub">
    <HPageHeader title="Hub" :description="hub?.joined ? `${summary.synced ?? 0} in sync on ${host}` : undefined">
      <div class="hv-actions">
        <HButton variant="secondary" size="compact" :disabled="checking" @click="emit('check')">
          <RefreshCw :size="15" :class="{ 'hv-spin': checking }" />
          <span>{{ fresh ? 'Check now' : 'Retry' }}</span>
        </HButton>
        <HButton
          v-if="summary.to_push" variant="secondary" size="compact" :disabled="!fresh"
          :aria-label="`Push ${summary.to_push}${fresh ? '' : ' (unavailable: the hub cannot be reached)'}`"
          :title="fresh ? '' : 'The hub cannot be reached'" @click="emit('push-needed')"
        >
          <ArrowUp :size="15" /><span>Push {{ summary.to_push }}</span>
        </HButton>
        <HButton
          v-if="summary.to_pull" variant="secondary" size="compact" :disabled="!fresh"
          :aria-label="`Pull ${summary.to_pull}${fresh ? '' : ' (unavailable: the hub cannot be reached)'}`"
          :title="fresh ? '' : 'The hub cannot be reached'" @click="emit('pull-all')"
        >
          <ArrowDown :size="15" /><span>Pull {{ summary.to_pull }}</span>
        </HButton>
      </div>
    </HPageHeader>

    <HEmptyState v-if="!hub?.joined" title="No hub joined" description="This machine has not joined a hub. Run: ASM_JOIN_TOKEN=… asm join <url>" />

    <template v-else>
      <div class="hv-grid">
        <HCard title="Connection">
          <div class="hv-stack">
            <HConnectionState :state="connState" :label="connLabel" :retryable="!fresh && !checking" @retry="emit('check')" />
            <HDescriptionList :items="connItems" />
            <HAlert
              v-if="hub.error && !checking" tone="warning"
              :description="hub.error + (hub.stale ? ` The sessions below are as of ${when(checkedAt)}.` : '')"
            >
              <small v-if="hub.detail" class="hv-mono">{{ hub.detail }}</small>
            </HAlert>
          </div>
        </HCard>

        <HCard title="Daemon">
          <div v-if="daemon" class="hv-stack">
            <div><HBadge :tone="daemonTone" dot>{{ daemonLabel }}</HBadge></div>
            <HDescriptionList v-if="daemonItems.length" :items="daemonItems" />
            <p v-if="daemon.state !== 'running'" class="hv-faint"><code class="hv-mono">asm daemon start</code> keeps the hub up to date.</p>
            <HAlert v-if="daemon.file?.last_error" tone="warning" :description="daemon.file.last_error" />
            <HTimeline v-if="recent.length" label="Recent daemon activity" :items="recent" />
          </div>
          <p v-else class="hv-faint">No daemon information.</p>
        </HCard>

        <HCard :title="`Machines (${machines.length})`">
          <div class="hv-stack">
            <HList v-if="machines.length" label="Machines on the hub">
              <HListItem
                v-for="m in shownMachines" :key="m.id" :title="m.name"
                :description="m.last_seen ? 'seen ' + when(m.last_seen) : 'never seen'"
                :badge="m.name === hub.machine ? 'this machine' : undefined"
              >
                <template #leading><Monitor :size="16" aria-hidden="true" /></template>
              </HListItem>
            </HList>
            <HButton v-if="machines.length > 5" variant="ghost" size="compact" :aria-expanded="showAllMachines" @click="showAllMachines = !showAllMachines">
              {{ showAllMachines ? 'Show fewer' : `Show all ${machines.length}` }}
            </HButton>
            <p v-if="!machines.length" class="hv-faint">The hub did not list its machines.</p>
          </div>
        </HCard>
      </div>

      <div class="hv-tools">
        <HInput v-model="query" type="search" label="Filter hub sessions" placeholder="Title, id, project, machine…" />
        <HSwitch v-model="onlyAttention" label="Only what needs doing" />
      </div>
      <HChipGroup v-if="stateOptions.length" v-model="picked" :options="stateOptions" label="Filter by sync state" />

      <HEmptyState
        v-if="!shown.length"
        :title="rows.length ? 'Nothing matches these filters' : 'Nothing to sync'"
        :description="rows.length ? 'Clear a filter to see more.' : 'Nothing on the hub, and nothing here to send.'"
      />
      <HList v-else-if="narrow" label="Sessions on this machine and on the hub">
        <HListItem
          v-for="r in shown" :key="rowKey(r)" interactive :title="r.title || 'Untitled session'"
          :description="`${dir(r.project)} · ${where(r)} · ${when(r.updated)}`" @activate="detailKey = rowKey(r)"
        >
          <template #leading><AgentMark :agent="r.agent" :size="15" /></template>
          <template #trailing>
            <HBadge :tone="hubTone(r.state)"><component :is="iconOf(r)" :size="12" aria-hidden="true" /> {{ r.label }}</HBadge>
          </template>
        </HListItem>
      </HList>
      <HDataTable
        v-else v-model:page="page" label="Sessions on this machine and on the hub"
        :rows="tableRows" :columns="columns" :page-size="25"
      >
          <template v-for="r in shown" :key="'s'+rowKey(r)" #[cell(r,`sync`)]>
            <HBadge :tone="hubTone(r.state)" :title="r.hint" :style="hub.stale ? 'opacity:.75' : ''">
              <component :is="iconOf(r)" :size="12" aria-hidden="true" /> {{ r.label }}
            </HBadge>
          </template>
          <template v-for="r in shown" :key="'n'+rowKey(r)" #[cell(r,`session`)]>
            <button class="hv-open" type="button" :aria-label="`Details for ${r.title || r.short_id}`" @click="detailKey = rowKey(r)">
              <span class="hv-session">
                <AgentMark :agent="r.agent" :size="15" />
                <span class="hv-stitle">
                  <span :title="r.title || 'Untitled session'">{{ r.title || 'Untitled session' }}</span>
                  <span class="hv-id">{{ r.short_id }}</span>
                </span>
              </span>
            </button>
          </template>
          <template v-for="r in shown" :key="'p'+rowKey(r)" #[cell(r,`project`)]>
            <span class="hv-proj" :title="dir(r.project)">{{ dir(r.project) }}</span>
          </template>
          <template v-for="r in shown" :key="'a'+rowKey(r)" #[cell(r,`act`)]>
            <div class="hv-rowact">
              <HButton
                v-if="buttonOf(r)" variant="secondary" size="compact" :disabled="!!buttonOf(r).off"
                :title="buttonOf(r).off || r.hint"
                :aria-label="`${buttonOf(r).text} ${r.title || r.short_id}${buttonOf(r).off ? ' (unavailable: ' + buttonOf(r).off + ')' : ''}`"
                @click="press(r)"
              >
                <ArrowUp v-if="r.action === 'push'" :size="14" />
                <ArrowUpDown v-else-if="r.action === 'resolve'" :size="14" />
                <Scale v-else-if="buttonOf(r).compare" :size="14" />
                <ArrowDown v-else :size="14" />
                <span>{{ buttonOf(r).text }}</span>
              </HButton>
            </div>
          </template>
      </HDataTable>

      <HSheet :open="sheetOpen" :title="detail?.title || 'Untitled session'" :description="detail?.hint" width="520px" @close="detailKey = null">
        <div v-if="detail" class="hv-sheet">
          <div><HBadge :tone="hubTone(detail.state)">{{ detail.label }}</HBadge></div>
          <HDescriptionList :items="detailItems" />
          <HCopyField v-if="command(detail)" label="Same from a terminal" :value="command(detail)" />
        </div>
        <template #footer>
          <HButton v-if="detail && buttonOf(detail) && !buttonOf(detail).resolve" variant="primary" :disabled="!!buttonOf(detail).off" :title="buttonOf(detail).off" @click="press(detail)">
            {{ buttonOf(detail).text }}
          </HButton>
          <HButton variant="ghost" @click="detailKey = null">Close</HButton>
        </template>
      </HSheet>
    </template>
  </section>
</template>
