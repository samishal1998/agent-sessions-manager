<script setup>
import { computed, onMounted, ref } from 'vue'
import {
  HAlert, HBadge, HButton, HCard, HCopyField, HDataTable, HEmptyState, HInput, HPageHeader, HStatCard, HTabs, HTheme, HThemeSwitcher, HTimeline,
  HToaster, tableCellSlot,
} from '@hearth-ui/vue'
import AgentMark from './components/AgentMark.vue'
import DialogHost from './components/DialogHost.vue'
import { confirmDialog } from './dialogs.js'
import { dismiss, notify, toasts } from './toasts.js'
import { useTheme } from './useTheme.js'
import { AdminError, admin, saveToken, savedToken } from './admin-api.js'
import { ago, bytes } from './format.js'
import { shortId } from './ids.js'
const sid = (s) => shortId({ ref: { agent: s.agent, native_id: s.id }, slug: s.slug })
import './styles/admin.css'

// The hub's own page: who has joined, what is stored, and the few actions an
// owner needs. It controls the hub, never a machine's agents.
const { preference, resolved, tokens, setPreference } = useTheme()
const token = ref(savedToken())
const input = ref('')
const signedIn = ref(false)
const error = ref('')
const busy = ref(false)
const tab = ref('overview')
const host = location.host

const overview = ref(null)
const machines = ref([])
const sessions = ref([])
const log = ref([])
const query = ref('')
const preview = ref(null)

const api = computed(() => admin(token.value))

async function load() {
  busy.value = true
  error.value = ''
  try {
    // The token is proven by the first call alone, so a wrong one is one 401.
    const o = await api.value.overview()
    const [m, s, l] = await Promise.all([api.value.machines(), api.value.sessions(), api.value.log()])
    overview.value = o
    machines.value = m
    sessions.value = s
    log.value = l
    signedIn.value = true
  } catch (e) {
    signedIn.value = false
    if (e instanceof AdminError && e.status === 401) {
      saveToken('')
      token.value = ''
      error.value = 'That token was not accepted. If this hub has no admin token yet, run `asm hub admin-token` on the hub machine.'
    } else {
      error.value = `Could not reach the hub: ${e.message}`
    }
  } finally {
    busy.value = false
  }
}

async function signIn() {
  const t = input.value.trim()
  if (!t) return
  token.value = t
  saveToken(t)
  input.value = ''
  await load()
  if (!signedIn.value) error.value ||= 'That token was not accepted.'
}
function signOut() {
  saveToken('')
  token.value = ''
  signedIn.value = false
  overview.value = null
  error.value = ''
}

async function run(what, fn) {
  try {
    const r = await fn()
    notify(what)
    await load()
    return r
  } catch (e) {
    if (e instanceof AdminError && e.status === 401) return load().then(() => undefined)
    notify(`${what.replace(/\.$/, '')} failed`, { tone: 'danger', description: e.message })
  }
}

async function revoke(m) {
  if (await confirmDialog({ title: `Revoke ${m.name}?`, description: 'Its credential stops working at once. Its sessions stay on the hub.', confirmLabel: 'Revoke', danger: true }))
    run(`Revoked ${m.name}.`, () => api.value.revoke(m.id))
}
async function rotateJoin() {
  if (await confirmDialog({ title: 'Replace the join token?', description: 'The old one stops working. Machines that already joined keep their access.', confirmLabel: 'Replace token', danger: true }))
    run('Join token replaced.', () => api.value.rotateJoin())
}
async function remove(s) {
  const name = s.title || s.id
  if (await confirmDialog({ title: `Delete ${name} from the hub?`, description: `All ${s.revisions} revision(s) go. Machines that have it keep their copy. Files nothing else uses stay until you collect them.`, confirmLabel: 'Delete', danger: true }))
    run(`Deleted ${name}.`, () => api.value.remove(s.agent, s.id))
}
async function previewCollect() {
  preview.value = null
  try {
    preview.value = await api.value.collect(true)
  } catch (e) {
    notify('Preview failed', { tone: 'danger', description: e.message })
  }
}
async function collect() {
  const p = preview.value
  if (!(await confirmDialog({ title: 'Remove unused files?', description: `${p.removed} file(s), ${bytes(p.freed_bytes)}.`, confirmLabel: 'Remove files', danger: true }))) return
  const r = await run('Collected unused files.', () => api.value.collect(false))
  if (r) preview.value = null
}

const joinCommand = computed(() => (overview.value ? `ASM_JOIN_TOKEN=${overview.value.join_token} asm join ${location.protocol}//${location.host}` : ''))

const shown = computed(() => {
  const q = query.value.trim().toLowerCase()
  return sessions.value.filter((s) => !q || [s.title, s.id, s.project, s.machine, s.agent].filter(Boolean).some((x) => String(x).toLowerCase().includes(q)))
})

const tabs = computed(() => [
  { value: 'overview', label: 'Overview' },
  { value: 'machines', label: `Machines (${machines.value.length})` },
  { value: 'sessions', label: `Sessions (${sessions.value.length})` },
  { value: 'storage', label: 'Storage' },
  { value: 'activity', label: 'Activity' },
])

const machineCols = [
  { key: 'name', label: 'Name' }, { key: 'sessions', label: 'Sessions pushed' }, { key: 'joined', label: 'Joined' },
  { key: 'seen', label: 'Last seen' }, { key: 'actions', label: 'Actions' },
]
const machineRows = computed(() => machines.value.map((m) => ({ id: m.id, name: m.name, sessions: m.sessions, joined: ago(m.joined), seen: m.last_seen ? ago(m.last_seen) : 'never', actions: '' })))
const sessionCols = [
  { key: 'title', label: 'Session', width: '34%' }, { key: 'project', label: 'Project', width: '18%' }, { key: 'machine', label: 'From' },
  { key: 'revisions', label: 'Revisions' }, { key: 'size', label: 'Size' }, { key: 'pushed', label: 'Pushed' }, { key: 'actions', label: 'Actions' },
]
const sessionRows = computed(() =>
  shown.value.map((s) => ({ id: `${s.agent}/${s.id}`, title: s.title || 'Untitled session', project: (s.project || '').split('/').filter(Boolean).pop() || '—', machine: s.machine || '—', revisions: s.revisions, size: bytes(s.size), pushed: ago(s.pushed_at), actions: '' })),
)
const byKey = computed(() => Object.fromEntries(sessions.value.map((s) => [`${s.agent}/${s.id}`, s])))
const byMachine = computed(() => Object.fromEntries(machines.value.map((m) => [m.id, m])))
const events = computed(() => log.value.slice(0, 20).map((e, i) => ({ id: String(i), title: e.action, description: e.target, timestamp: ago(e.at) })))

onMounted(() => token.value && load())
</script>

<template>
  <HTheme class="asm-root" theme="dusk" :mode="resolved" :data-asm-mode="resolved" :tokens="tokens">
    <main class="admin">
      <HPageHeader title="asm hub administration" :description="`Machines, the join token and stored sessions on ${host}.`">
        <HThemeSwitcher :model-value="preference" label="Color mode" @update:model-value="setPreference" />
        <template v-if="signedIn">
          <HButton size="compact" :loading="busy" label="Refresh" @click="load" />
          <HButton size="compact" variant="ghost" label="Sign out" @click="signOut" />
        </template>
      </HPageHeader>

      <HAlert v-if="error" tone="warning" :description="error" />

      <HCard v-if="!signedIn" class="admin-login" title="Sign in" description="Paste the admin token printed by `asm hub admin-token` on this hub's machine.">
        <form class="admin-form" @submit.prevent="signIn">
          <HInput v-model="input" type="password" label="Admin token" autocomplete="off" placeholder="asma_…" required />
          <HButton type="submit" variant="primary" :loading="busy" label="Sign in" />
        </form>
        <p>This page controls the hub itself: machines, the join token, stored sessions. It cannot act on anyone's agents. Use it over HTTPS or on a trusted network, because the token is sent with each request.</p>
      </HCard>

      <HTabs v-else-if="overview" v-model="tab" :items="tabs" label="Hub sections" variant="underline">
        <template #overview>
          <div class="admin-stack">
            <div class="admin-stats">
              <HStatCard label="Machines" :value="overview.stats.machines" icon="server" />
              <HStatCard label="Sessions" :value="overview.stats.sessions" :detail="`/ ${overview.stats.revisions} revisions`" icon="layout" />
              <HStatCard label="Stored files" :value="bytes(overview.stats.blob_bytes)" :detail="`/ ${overview.stats.blobs} files, up to ${bytes(overview.max_blob_bytes)} each`" icon="providers" />
            </div>
            <HCard title="Join another machine" description="Anyone with this token can add a machine that can read every session on the hub.">
              <div class="admin-stack">
                <HCopyField label="Join command" :value="joinCommand" secret @copied="notify('Join command copied.')" />
                <div><HButton size="compact" variant="danger" label="Replace the join token" @click="rotateJoin" /></div>
              </div>
            </HCard>
          </div>
        </template>

        <template #machines>
          <ul class="admin-cards" aria-label="Machines that have joined this hub">
            <li v-for="m in machines" :key="m.id" class="admin-card">
              <strong class="admin-clip" :title="m.name">{{ m.name }}</strong>
              <span class="admin-meta">{{ m.sessions }} sessions pushed · joined {{ ago(m.joined) }} · seen {{ m.last_seen ? ago(m.last_seen) : 'never' }}</span>
              <HButton size="compact" variant="danger" label="Revoke" :aria-label="`Revoke ${m.name}`" @click="revoke(m)" />
            </li>
            <li v-if="!machines.length" class="admin-meta">No machine has joined.</li>
          </ul>
          <HDataTable class="admin-table" label="Machines that have joined this hub" :rows="machineRows" :columns="machineCols" empty-text="No machine has joined.">
            <template v-for="m in machineRows" :key="m.id" #[tableCellSlot(m.id,'actions')]>
              <HButton size="compact" variant="danger" label="Revoke" :aria-label="`Revoke ${m.name}`" @click="revoke(byMachine[m.id])" />
            </template>
          </HDataTable>
        </template>

        <template #sessions>
          <div class="admin-stack">
            <HInput v-model="query" type="search" label="Filter sessions" placeholder="Title, id, project, machine…" />
            <ul class="admin-cards" aria-label="Sessions stored on this hub">
              <li v-for="s in shown" :key="s.agent + s.id" class="admin-card">
                <span class="admin-session"><AgentMark :agent="s.agent" :size="15" /> <strong class="admin-clip" :title="s.title">{{ s.title || 'Untitled session' }}</strong></span>
                <span class="admin-meta"><span class="admin-mono">{{ sid(s) }}</span> · <span class="admin-clip" :title="s.project">{{ (s.project || '').split('/').filter(Boolean).pop() || '—' }}</span> · from {{ s.machine || '—' }}</span>
                <span class="admin-meta">{{ bytes(s.size) }} · {{ s.revisions }} {{ s.revisions === 1 ? "revision" : "revisions" }} · {{ ago(s.pushed_at) }}</span>
                <HButton size="compact" variant="danger" label="Delete" :aria-label="`Delete ${s.title || s.id}`" @click="remove(s)" />
              </li>
              <li v-if="!shown.length" class="admin-meta">{{ sessions.length ? 'Nothing matches.' : 'Nothing has been pushed yet.' }}</li>
            </ul>
            <HDataTable class="admin-table" label="Sessions stored on this hub" :rows="sessionRows" :columns="sessionCols" :empty-text="sessions.length ? 'Nothing matches.' : 'Nothing has been pushed yet.'">
              <template v-for="r in sessionRows" :key="r.id" #[tableCellSlot(r.id,'title')]>
                <span class="admin-session"><AgentMark :agent="byKey[r.id].agent" :size="15" /> <span class="admin-clip" :title="r.title">{{ r.title }}</span> <span class="admin-mono">{{ sid(byKey[r.id]) }}</span></span>
              </template>
              <template v-for="r in sessionRows" :key="r.id + 'a'" #[tableCellSlot(r.id,'actions')]>
                <HButton size="compact" variant="danger" label="Delete" :aria-label="`Delete ${r.title}`" @click="remove(byKey[r.id])" />
              </template>
            </HDataTable>
          </div>
        </template>

        <template #storage>
          <HCard title="Storage" description="Files stay on disk after their session is deleted or a push fails. Collecting removes the ones no revision uses (never one uploaded in the last hour).">
            <div class="admin-actions">
              <HButton size="compact" label="Preview" @click="previewCollect" />
              <HButton v-if="preview && preview.removed" size="compact" variant="danger" :label="`Remove ${preview.removed} file(s), ${bytes(preview.freed_bytes)}`" @click="collect" />
              <HBadge v-if="preview && !preview.removed" tone="neutral" label="Nothing to collect" />
            </div>
          </HCard>
        </template>

        <template #activity>
          <HTimeline label="What an administrator did on this hub" :items="events" empty-text="Nothing yet." />
        </template>
      </HTabs>
    </main>
    <DialogHost />
    <HToaster :items="toasts" @dismiss="dismiss" />
  </HTheme>
</template>
