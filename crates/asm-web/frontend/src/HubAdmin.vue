<script setup>
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import {
  HAlert, HBadge, HButton, HCard, HCopyField, HDataTable, HInput, HPageHeader, HSkeleton, HTabs, HTheme, HTimeline,
  HToaster, tableCellSlot,
} from '@hearth-ui/vue'
import { Ban, Check, Power, TriangleAlert } from 'lucide-vue-next'
import ColorModeSwitch from './components/ColorModeSwitch.vue'
import SessionsPanel from './components/admin/SessionsPanel.vue'
import CommandsPanel from './components/admin/CommandsPanel.vue'
import NewCommandDialog from './components/admin/NewCommandDialog.vue'
import StatusBadge from './components/admin/StatusBadge.vue'
import TranscriptSheet from './components/admin/TranscriptSheet.vue'
import DialogHost from './components/DialogHost.vue'
import { confirmDialog } from './dialogs.js'
import { dismiss, notify, toasts } from './toasts.js'
import { useTheme } from './useTheme.js'
import { AdminError, admin, saveToken, savedToken } from './admin-api.js'
import { isOpen, remoteStatus } from './commands.js'
import { ago, bytes } from './format.js'
import { shortId } from './ids.js'
const sid = (s) => shortId({ ref: { agent: s.agent, native_id: s.id }, slug: s.slug })
import './styles/admin.css'

// The hub's own page: who has joined, what is stored, and the few actions an
// owner needs. It controls the hub, never a machine's agents.
const { resolved, tokens } = useTheme()
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
const commands = ref([])
const commandsError = ref('')
const newCmd = ref(null) // null closed, { session } open
const opened = ref(null)
const refreshed = ref(null)
const preview = ref(null)

const api = computed(() => admin(token.value))

async function load() {
  busy.value = true
  error.value = ''
  try {
    // The token is proven by the first call alone, so a wrong one is one 401.
    const o = await api.value.overview()
    const [m, s, l] = await Promise.all([api.value.machines(), api.value.sessions(), api.value.log(), loadCommands()])
    overview.value = o
    machines.value = m
    sessions.value = s
    log.value = l
    signedIn.value = true
    refreshed.value = new Date()
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

// A hub without remote control answers the commands route with an error; that
// must not take the rest of the page down, so only a 401 is rethrown.
async function loadCommands() {
  try {
    commands.value = await api.value.commands(100)
    commandsError.value = ''
  } catch (e) {
    if (e instanceof AdminError && e.status === 401) throw e
    commandsError.value = e instanceof AdminError && e.status === 404 ? 'This hub does not support remote control yet. Update asm on the hub machine.' : e.message
  }
}
async function reloadCommands() {
  try {
    await loadCommands()
  } catch {
    load()
  }
}
function created(c) {
  newCmd.value = null
  notify(`Queued: ${c.op} ${c.title || 'the session'} ${c.op === 'push' ? 'from' : 'to'} ${c.machine.name}.`)
  tab.value = 'commands'
  reloadCommands()
}
// While commands are in flight the list keeps itself fresh; otherwise it is left alone.
const poll = setInterval(() => {
  if (tab.value === 'commands' && signedIn.value && document.visibilityState === 'visible' && commands.value.some(isOpen)) reloadCommands()
}, 10000)
onBeforeUnmount(() => clearInterval(poll))

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
    run(`Deleted ${name}.`, () => api.value.remove(s.agent, s.id)).then((r) => r && (opened.value = null))
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

const stats = computed(() => {
  const o = overview.value.stats
  return [
    { label: 'Machines', value: o.machines },
    { label: 'Sessions', value: o.sessions, detail: `${o.revisions} revisions stored` },
    { label: 'Stored files', value: bytes(o.blob_bytes), detail: `${o.blobs} files, up to ${bytes(overview.value.max_blob_bytes)} each` },
  ]
})
const tabs = computed(() => [
  { value: 'overview', label: 'Overview' },
  { value: 'machines', label: `Machines (${machines.value.length})` },
  { value: 'commands', label: `Commands (${commands.value.length})` },
  { value: 'sessions', label: `Sessions (${sessions.value.length})` },
  { value: 'storage', label: 'Storage' },
  { value: 'activity', label: 'Activity' },
])

const machineCols = [
  { key: 'name', label: 'Name' }, { key: 'remote', label: 'Remote control' }, { key: 'sessions', label: 'Sessions pushed' }, { key: 'joined', label: 'Joined' },
  { key: 'seen', label: 'Last seen' }, { key: 'actions', label: 'Actions' },
]
const REMOTE_ICON = { on: Check, stale: TriangleAlert, off: Power, unknown: Ban }
const remote = (m) => ({ ...remoteStatus(m.remote), ops: m.remote?.enabled && m.remote.ops?.length ? `Allowed: ${m.remote.ops.join(', ')}` : '' })
const machineRows = computed(() => machines.value.map((m) => ({ id: m.id, name: m.name, sessions: m.sessions, joined: ago(m.joined), seen: m.last_seen ? ago(m.last_seen) : 'never', remote: remote(m).label, actions: '' })))
const byMachine = computed(() => Object.fromEntries(machines.value.map((m) => [m.id, m])))
const events = computed(() => log.value.slice(0, 20).map((e, i) => ({ id: String(i), title: e.action, description: e.target, timestamp: ago(e.at) })))

onMounted(() => token.value && load())
</script>

<template>
  <HTheme class="asm-root" theme="dusk" :mode="resolved" :data-asm-mode="resolved" :tokens="tokens">
    <main class="admin">
      <HPageHeader title="asm hub administration" :description="`Machines, the join token and stored sessions on ${host}.`">
        <div class="admin-bar">
          <span v-if="signedIn && refreshed" class="admin-stamp">Updated {{ refreshed.toLocaleTimeString() }}</span>
          <ColorModeSwitch />
          <template v-if="signedIn">
            <HButton size="compact" :loading="busy" label="Refresh" @click="load" />
            <HButton size="compact" variant="ghost" label="Sign out" @click="signOut" />
          </template>
        </div>
      </HPageHeader>

      <HAlert v-if="error" tone="warning" :description="error" />

      <HCard v-if="!signedIn" class="admin-login" title="Sign in" description="Paste the admin token printed by `asm hub admin-token` on this hub's machine.">
        <form class="admin-form" @submit.prevent="signIn">
          <HInput v-model="input" type="password" label="Admin token" autocomplete="off" placeholder="asma_…" required />
          <HButton type="submit" variant="primary" :loading="busy" label="Sign in" />
        </form>
        <p>This page controls the hub itself: machines, the join token, stored sessions. It cannot act on anyone's agents. Use it over HTTPS or on a trusted network, because the token is sent with each request.</p>
      </HCard>

      <HSkeleton v-else-if="!overview" :lines="5" label="Loading the hub" />

      <HTabs v-else v-model="tab" :items="tabs" label="Hub sections" variant="underline">
        <template #overview>
          <div class="admin-stack">
            <div class="admin-stats">
              <HCard v-for="c in stats" :key="c.label">
                <dl class="admin-stat"><dt>{{ c.label }}</dt><dd class="admin-stat-value">{{ c.value }}</dd><dd v-if="c.detail" class="admin-stat-detail">{{ c.detail }}</dd></dl>
              </HCard>
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
              <span class="admin-actions"><span class="admin-meta">Remote control</span><StatusBadge :tone="remote(m).tone" :label="remote(m).label" :icon="REMOTE_ICON[remote(m).kind]" /></span>
              <span v-if="remote(m).ops" class="admin-meta">{{ remote(m).ops }}</span>
              <span class="admin-meta">{{ m.sessions }} sessions pushed · joined {{ ago(m.joined) }} · seen {{ m.last_seen ? ago(m.last_seen) : 'never' }}</span>
              <HButton size="compact" variant="danger" label="Revoke" :aria-label="`Revoke ${m.name}`" @click="revoke(m)" />
            </li>
            <li v-if="!machines.length" class="admin-meta">No machine has joined.</li>
          </ul>
          <HDataTable class="admin-table" label="Machines that have joined this hub" :rows="machineRows" :columns="machineCols" empty-text="No machine has joined.">
            <template v-for="m in machineRows" :key="m.id + 'r'" #[tableCellSlot(m.id,'remote')]>
              <span class="cp-status"><StatusBadge :tone="remote(byMachine[m.id]).tone" :label="remote(byMachine[m.id]).label" :icon="REMOTE_ICON[remote(byMachine[m.id]).kind]" /><span v-if="remote(byMachine[m.id]).ops" class="admin-meta">{{ remote(byMachine[m.id]).ops }}</span></span>
            </template>
            <template v-for="m in machineRows" :key="m.id" #[tableCellSlot(m.id,'actions')]>
              <HButton size="compact" variant="danger" label="Revoke" :aria-label="`Revoke ${m.name}`" @click="revoke(byMachine[m.id])" />
            </template>
          </HDataTable>
        </template>

        <template #commands>
          <CommandsPanel :commands="commands" :machines="machines" :api="api" :error="commandsError" @new="newCmd = {}" @changed="reloadCommands" @expired="load" />
        </template>

        <template #sessions>
          <SessionsPanel :sessions="sessions" :sid="sid" @open="opened = $event" @remove="remove" @send="newCmd = { session: $event }" />
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
    <TranscriptSheet v-if="signedIn" :session="opened" :api="api" :sid="opened ? sid(opened) : ''" @close="opened = null" @remove="remove" @expired="opened = null; load()" />
    <NewCommandDialog v-if="signedIn && newCmd" :sessions="sessions" :machines="machines" :api="api" :prefill="newCmd.session || null" @close="newCmd = null" @created="created" @expired="newCmd = null; load()" />
    <DialogHost />
    <HToaster :items="toasts" @dismiss="dismiss" />
  </HTheme>
</template>
