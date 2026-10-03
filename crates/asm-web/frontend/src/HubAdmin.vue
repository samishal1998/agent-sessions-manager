<script setup>
import { computed, onMounted, ref } from 'vue'
import { HAlert, HBadge, HButton, HCard, HTheme } from '@hearth-ui/vue'
import { LogOut, RefreshCw, Search, ShieldCheck, Trash2, Copy, Check } from 'lucide-vue-next'
import AgentMark from './components/AgentMark.vue'
import { NIGHT_OWL } from './theme.js'
import { AdminError, admin, saveToken, savedToken } from './admin-api.js'
import { ago, bytes } from './format.js'

// The hub's own page: who has joined, what is stored, and the few actions an
// owner needs. It controls the hub, never a machine's agents.
const token = ref(savedToken())
const input = ref('')
const signedIn = ref(false)
const error = ref('')
const busy = ref(false)
const notice = ref('')

const overview = ref(null)
const machines = ref([])
const sessions = ref([])
const log = ref([])
const query = ref('')
const preview = ref(null)
const copied = ref(false)

const api = computed(() => admin(token.value))

async function load() {
  busy.value = true
  error.value = ''
  try {
    // The token is proven by the first call alone, so a wrong one is one 401,
    // not four.
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
      error.value = savedToken() || input.value ? 'That token was not accepted. If this hub has no admin token yet, run `asm hub admin-token` on the hub machine.' : ''
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
}

async function run(what, fn) {
  notice.value = ''
  error.value = ''
  try {
    const r = await fn()
    notice.value = what
    await load()
    return r
  } catch (e) {
    error.value = `${what} failed: ${e.message}`
  }
}

function revoke(m) {
  if (confirm(`Revoke ${m.name}?\n\nIts credential stops working at once. Its sessions stay on the hub.`))
    run(`Revoked ${m.name}.`, () => api.value.revoke(m.id))
}
function rotateJoin() {
  if (confirm('Replace the join token?\n\nThe old one stops working. Machines that already joined keep their access.'))
    run('Join token replaced.', () => api.value.rotateJoin())
}
function remove(s) {
  if (
    confirm(
      `Delete ${s.title || s.id} from the hub?\n\nAll ${s.revisions} revision(s) go. Machines that have it keep their copy. Files nothing else uses stay until you collect them.`,
    )
  )
    run(`Deleted ${s.title || s.id}.`, () => api.value.remove(s.agent, s.id))
}
async function previewCollect() {
  preview.value = null
  try {
    preview.value = await api.value.collect(true)
  } catch (e) {
    error.value = `Preview failed: ${e.message}`
  }
}
async function collect() {
  if (!confirm(`Remove ${preview.value.removed} unused file(s), ${bytes(preview.value.freed_bytes)}?`)) return
  const r = await run('Collected unused files.', () => api.value.collect(false))
  if (r) preview.value = null
}

const host = computed(() => location.host)
const joinCommand = computed(() => (overview.value ? `ASM_JOIN_TOKEN=${overview.value.join_token} asm join ${location.protocol}//${location.host}` : ''))
async function copyJoin() {
  try {
    await navigator.clipboard.writeText(joinCommand.value)
    copied.value = true
    setTimeout(() => (copied.value = false), 1500)
  } catch {
    /* the command is on screen to select */
  }
}

const shown = computed(() => {
  const q = query.value.trim().toLowerCase()
  return sessions.value.filter(
    (s) => !q || [s.title, s.id, s.project, s.machine, s.agent].filter(Boolean).some((x) => String(x).toLowerCase().includes(q)),
  )
})
const when = (t) => ago(t)

onMounted(() => token.value && load())
</script>

<template>
  <HTheme class="asm-root" theme="dusk" mode="dark" :tokens="NIGHT_OWL">
    <main class="admin">
      <header class="admin-head">
        <ShieldCheck :size="22" />
        <h1 class="admin-title"><span>asm hub</span> administration</h1>
        <span class="faint mono">{{ host }}</span>
        <span class="hv-spacer" />
        <template v-if="signedIn">
          <HButton size="compact" :loading="busy" @click="load"><RefreshCw :size="15" /> Refresh</HButton>
          <HButton size="compact" variant="ghost" @click="signOut"><LogOut :size="15" /> Sign out</HButton>
        </template>
      </header>

      <HAlert v-if="error" tone="warning" :description="error" />
      <HAlert v-if="notice && signedIn" tone="success" :description="notice" dismissible @dismiss="notice = ''" />

      <HCard v-if="!signedIn" class="admin-login" title="Sign in" description="Paste the admin token printed by `asm hub admin-token` on this hub's machine.">
        <form class="admin-form" @submit.prevent="signIn">
          <label class="admin-field">
            <span>Admin token</span>
            <input v-model="input" type="password" autocomplete="off" spellcheck="false" placeholder="asma_…" required />
          </label>
          <HButton type="submit" variant="primary">Sign in</HButton>
        </form>
        <p class="faint">This page controls the hub itself — machines, the join token, stored sessions. It cannot act on anyone's agents. Use it over HTTPS or on a trusted network: the token is sent with each request.</p>
      </HCard>

      <template v-else-if="overview">
        <div class="hv-cards">
          <HCard class="hv-card" title="Machines"><p class="admin-num">{{ overview.stats.machines }}</p></HCard>
          <HCard class="hv-card" title="Sessions"><p class="admin-num">{{ overview.stats.sessions }}</p><p class="faint">{{ overview.stats.revisions }} revisions</p></HCard>
          <HCard class="hv-card" title="Stored files"><p class="admin-num">{{ bytes(overview.stats.blob_bytes) }}</p><p class="faint">{{ overview.stats.blobs }} files · up to {{ bytes(overview.max_blob_bytes) }} each</p></HCard>
        </div>

        <HCard title="Join another machine" description="Anyone with this token can add a machine that can read every session on the hub.">
          <p class="hv-cmd">
            <code class="mono">{{ joinCommand }}</code>
            <button class="icon-btn" aria-label="Copy the join command" @click="copyJoin"><Check v-if="copied" :size="14" /><Copy v-else :size="14" /></button>
            <span class="sr-only" role="status">{{ copied ? 'Copied' : '' }}</span>
          </p>
          <HButton size="compact" variant="danger" @click="rotateJoin">Replace the join token</HButton>
        </HCard>

        <HCard title="Machines" :description="`${machines.length} joined`">
          <p v-if="!machines.length" class="faint">No machine has joined.</p>
          <div v-else class="admin-scroll" role="region" aria-label="Machines" tabindex="0">
          <table class="hv-table">
            <caption class="sr-only">Machines that have joined this hub</caption>
            <thead><tr><th scope="col">Name</th><th scope="col">Sessions pushed</th><th scope="col">Joined</th><th scope="col">Last seen</th><th scope="col"><span class="sr-only">Actions</span></th></tr></thead>
            <tbody>
              <tr v-for="m in machines" :key="m.id" class="hv-row">
                <td><strong>{{ m.name }}</strong> <span class="faint mono">{{ m.id }}</span></td>
                <td>{{ m.sessions }}</td>
                <td>{{ when(m.joined) }}</td>
                <td>{{ m.last_seen ? when(m.last_seen) : 'never' }}</td>
                <td class="hv-act"><HButton size="compact" variant="danger" :aria-label="`Revoke ${m.name}`" @click="revoke(m)">Revoke</HButton></td>
              </tr>
            </tbody>
          </table>
          </div>
        </HCard>

        <HCard title="Sessions on the hub" :description="`${sessions.length} sessions`">
          <label class="field admin-filter">
            <Search :size="15" />
            <input v-model="query" placeholder="Filter by title, id, project, machine…" aria-label="Filter sessions" />
          </label>
          <p v-if="!shown.length" class="faint">{{ sessions.length ? 'Nothing matches.' : 'Nothing has been pushed yet.' }}</p>
          <div v-else class="admin-scroll" role="region" aria-label="Sessions on the hub" tabindex="0">
          <table class="hv-table">
            <caption class="sr-only">Sessions stored on this hub</caption>
            <thead><tr><th scope="col">Session</th><th scope="col">Project</th><th scope="col">From</th><th scope="col">Revisions</th><th scope="col">Size</th><th scope="col">Pushed</th><th scope="col"><span class="sr-only">Actions</span></th></tr></thead>
            <tbody>
              <tr v-for="s in shown" :key="s.agent + s.id" class="hv-row">
                <td>
                  <div class="hv-session">
                    <AgentMark :agent="s.agent" :size="15" />
                    <span class="hv-stitle"><span class="truncate" :title="s.title || s.id">{{ s.title || 'Untitled session' }}</span><span class="faint mono">{{ s.id.slice(0, 13) }}</span></span>
                  </div>
                </td>
                <td class="hv-col-project">{{ s.project }}</td>
                <td>{{ s.machine || '—' }}</td>
                <td>{{ s.revisions }}</td>
                <td>{{ bytes(s.size) }}</td>
                <td>{{ when(s.pushed_at) }}</td>
                <td class="hv-act"><HButton size="compact" variant="danger" :aria-label="`Delete ${s.title || s.id}`" @click="remove(s)"><Trash2 :size="14" /> Delete</HButton></td>
              </tr>
            </tbody>
          </table>
          </div>
        </HCard>

        <HCard title="Storage" description="Files stay on disk after their session is deleted or a push fails. Collecting removes the ones no revision uses (never one uploaded in the last hour).">
          <div class="admin-actions">
            <HButton size="compact" @click="previewCollect">Preview</HButton>
            <HButton v-if="preview && preview.removed" size="compact" variant="danger" @click="collect">Remove {{ preview.removed }} file(s), {{ bytes(preview.freed_bytes) }}</HButton>
            <span v-if="preview && !preview.removed" class="faint">Nothing to collect.</span>
          </div>
        </HCard>

        <HCard title="Recent activity" description="What an administrator did on this hub.">
          <p v-if="!log.length" class="faint">Nothing yet.</p>
          <ul v-else class="admin-log">
            <li v-for="(e, i) in log.slice(0, 20)" :key="i"><HBadge tone="neutral" :label="e.action" /> <span>{{ e.target }}</span> <span class="faint">{{ when(e.at) }}</span></li>
          </ul>
        </HCard>
      </template>
    </main>
  </HTheme>
</template>
