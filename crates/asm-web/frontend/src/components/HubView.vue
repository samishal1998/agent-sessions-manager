<script setup>
import { computed, onMounted, onUnmounted, ref } from 'vue'
import {
  ArrowDown,
  ArrowUp,
  ArrowUpDown,
  Check,
  ChevronRight,
  Cloud,
  CloudOff,
  Copy,
  Menu,
  Monitor,
  Radio,
  RefreshCw,
  Scale,
  Search,
  TriangleAlert,
} from 'lucide-vue-next'
import AgentMark from './AgentMark.vue'
import { ago, agoUnix, bytes } from '../format.js'
import { shortProject } from '../ids.js'

// Everything about how this machine stands with the hub, in one place: the
// connection, the daemon, the other machines, and every session here and on
// the hub with what each needs and why. The wording of states and next steps
// comes from the core (`RowState`), as everywhere else.
const props = defineProps({
  hub: { type: Object, default: null },
  checking: { type: Boolean, default: false },
  checkedAt: { type: Number, default: 0 },
  home: { type: String, default: null },
})
const emit = defineEmits(['check', 'push', 'pull', 'compare', 'push-needed', 'pull-all', 'menu'])

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

/* The table ------------------------------------------------------------ */
const ORDER = ['diverged', 'untracked', 'behind', 'remote', 'ahead', 'local', 'in_sync']
const PRIORITY = Object.fromEntries(ORDER.map((s, i) => [s, i]))
const query = ref('')
const picked = ref([])
const onlyAttention = ref(false)
const open = ref(new Set())

const stateChips = computed(() =>
  ORDER.map((state) => {
    const of = rows.value.filter((r) => r.state === state)
    return of.length ? { state, label: of[0].state_label, hint: of[0].hint, count: of.length } : null
  }).filter(Boolean),
)

const shown = computed(() => {
  const q = query.value.trim().toLowerCase()
  return rows.value
    .filter((r) => !picked.value.length || picked.value.includes(r.state))
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

function toggleState(state) {
  picked.value = picked.value.includes(state)
    ? picked.value.filter((s) => s !== state)
    : [...picked.value, state]
}
const rowKey = (r) => `${r.agent}:${r.id}`
// An id the DOM and aria-controls can use whatever characters a session id has.
const domId = (r) => 'hv-' + rowKey(r).replace(/[^A-Za-z0-9_-]/g, '_')
function toggleRow(r) {
  const next = new Set(open.value)
  next.has(rowKey(r)) ? next.delete(rowKey(r)) : next.add(rowKey(r))
  open.value = next
}

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
function press(r) {
  const b = buttonOf(r)
  if (b?.resolve) return toggleRow(r)
  if (b?.compare) return emit('compare', r)
  emit(r.action === 'push' ? 'push' : 'pull', r)
}

// The command that does the same from a terminal.
function command(r) {
  const id = r.short_id || r.id
  if (r.action === 'resolve') return `asm push ${id} --force`
  if (r.action === 'push') return `asm push ${id}`
  if (r.action === 'pull') return `asm pull ${id}`
  return ''
}
const copied = ref('')
async function copy(text) {
  try {
    await navigator.clipboard.writeText(text)
    copied.value = text
    setTimeout(() => (copied.value = ''), 1500)
  } catch {
    /* no clipboard (an http page that is not localhost): the text is on screen to select */
  }
}
// Where the session is, in a few words.
function where(r) {
  if (r.here && r.on_hub) return 'Both'
  return r.on_hub ? 'Hub only' : 'This machine only'
}
// "revision abc, 14 KB, pushed 3d ago by machB"
const hubCopy = (r) =>
  [`revision ${r.rev}`, bytes(r.size), r.pushed_at && `pushed ${when(r.pushed_at)}${r.machine ? ' by ' + r.machine : ''}`]
    .filter(Boolean)
    .join(', ')
const showAllMachines = ref(false)
const shownMachines = computed(() => (showAllMachines.value ? machines.value : machines.value.slice(0, 5)))
// The hub's portable form (`${HOME}/…`) reads as `~/…`, like local paths.
const dir = (p) => shortProject((p || '').replace('${HOME}', props.home || '~'), props.home)
const when = (ts) => ago(ts, now.value)
</script>

<template>
  <section class="hubview" aria-labelledby="hubview-title">
    <header class="hv-head">
      <button class="icon-btn mobile-only" aria-label="Show projects" @click="emit('menu')">
        <Menu :size="18" />
      </button>
      <h1 id="hubview-title" class="hv-title">Hub</h1>
      <span class="hv-spacer" />
      <button class="btn" :disabled="checking" @click="emit('check')">
        <RefreshCw :size="15" :class="{ spin: checking }" />
        <span>{{ fresh ? 'Check now' : 'Retry' }}</span>
      </button>
      <button
        v-if="summary.to_push"
        class="btn"
        :aria-disabled="!fresh || null"
        :aria-label="`Push ${summary.to_push}${fresh ? '' : ' (unavailable: the hub cannot be reached)'}`"
        :title="fresh ? '' : 'The hub cannot be reached'"
        @click="fresh && emit('push-needed')"
      >
        <ArrowUp :size="15" />
        <span>Push {{ summary.to_push }}</span>
      </button>
      <button
        v-if="summary.to_pull"
        class="btn"
        :aria-disabled="!fresh || null"
        :aria-label="`Pull ${summary.to_pull}${fresh ? '' : ' (unavailable: the hub cannot be reached)'}`"
        :title="fresh ? '' : 'The hub cannot be reached'"
        @click="fresh && emit('pull-all')"
      >
        <ArrowDown :size="15" />
        <span>Pull {{ summary.to_pull }}</span>
      </button>
    </header>

    <p v-if="!hub?.joined" class="empty">
      This machine has not joined a hub. Run <code class="mono">ASM_JOIN_TOKEN=… asm join &lt;url&gt;</code>.
    </p>

    <template v-else>
      <div class="hv-cards">
        <!-- Connection -->
        <div class="hv-card">
          <h2 class="hv-card-title">Connection</h2>
          <p class="hv-state" :class="fresh ? 'ok' : checking ? 'wait' : 'bad'" role="status">
            <RefreshCw v-if="checking" :size="15" class="spin" />
            <Cloud v-else-if="fresh" :size="15" />
            <CloudOff v-else :size="15" />
            <template v-if="checking">Checking…</template>
            <template v-else-if="fresh">Connected</template>
            <template v-else>Can't reach the hub</template>
          </p>
          <dl class="hv-dl">
            <dt>Hub</dt>
            <dd class="mono">{{ host }}</dd>
            <dt>This machine</dt>
            <dd>{{ hub.machine }}</dd>
            <dt>{{ fresh ? 'Checked' : 'Last reached' }}</dt>
            <dd>{{ checkedAt ? when(checkedAt) : 'not yet' }}</dd>
          </dl>
          <p v-if="hub.error && !checking" class="hv-problem" role="alert" :title="hub.detail">
            <TriangleAlert :size="15" />
            <span>
              {{ hub.error }}
              <template v-if="hub.stale"> The sessions below are as of {{ when(checkedAt) }}.</template>
            </span>
          </p>
        </div>

        <!-- Daemon -->
        <div class="hv-card">
          <h2 class="hv-card-title">Daemon</h2>
          <template v-if="daemon">
            <p
              class="hv-state"
              role="status"
              :class="daemon.state === 'running' && !daemon.file?.last_error ? 'ok' : daemon.state === 'not_running' ? 'wait' : 'bad'"
            >
              <Radio :size="15" />
              <template v-if="daemon.state === 'running'">Running</template>
              <template v-else-if="daemon.state === 'hung'">Not responding</template>
              <template v-else>Not running</template>
            </p>
            <dl v-if="daemon.file" class="hv-dl">
              <dt>Last push</dt>
              <dd>{{ agoUnix(daemon.file.last_push, now) || 'none yet' }}</dd>
              <dt>Last pass</dt>
              <dd>{{ agoUnix(daemon.file.last_pass, now) || 'none yet' }}</dd>
              <dt>Waiting</dt>
              <dd>{{ daemon.file.pending }} <span v-if="daemon.file.idle" class="faint">· {{ daemon.file.idle }} idle</span></dd>
              <dt>Pushes</dt>
              <dd>
                <template v-if="daemon.file.active_window">live or changed in the last {{ Math.round(daemon.file.active_window / 60) }} min</template>
                <template v-else>everything that differs</template>
              </dd>
              <dt>Every</dt>
              <dd>{{ daemon.file.interval }}s</dd>
            </dl>
            <p v-if="daemon.state !== 'running'" class="faint">
              <code class="mono">asm daemon start</code> keeps the hub up to date.
            </p>
            <p v-if="daemon.file?.last_error" class="hv-problem" role="alert">
              <TriangleAlert :size="15" />
              <span>{{ daemon.file.last_error }}</span>
            </p>
            <details v-if="daemon.file?.recent?.length" class="hv-recent">
              <summary>Recent ({{ daemon.file.recent.length }})</summary>
              <ul>
                <li v-for="(e, i) in daemon.file.recent.slice().reverse().slice(0, 8)" :key="i" :class="{ bad: !e.ok }">
                  <span class="faint">{{ agoUnix(e.at, now) }}</span> {{ e.line }}
                </li>
              </ul>
            </details>
          </template>
          <p v-else class="faint">No daemon information.</p>
        </div>

        <!-- Machines -->
        <div class="hv-card">
          <h2 class="hv-card-title">Machines <span class="faint">{{ machines.length }}</span></h2>
          <ul v-if="machines.length" class="hv-machines">
            <li v-for="m in shownMachines" :key="m.id">
              <Monitor :size="15" />
              <span class="hv-mname">{{ m.name }}</span>
              <span v-if="m.name === hub.machine" class="hv-tag">this machine</span>
              <span class="faint hv-seen">{{ m.last_seen ? 'seen ' + when(m.last_seen) : 'never seen' }}</span>
            </li>
          </ul>
          <button v-if="machines.length > 5" class="btn ghost" :aria-expanded="showAllMachines" @click="showAllMachines = !showAllMachines">
            {{ showAllMachines ? 'Show fewer' : `Show all ${machines.length}` }}
          </button>
          <p v-if="!machines.length" class="faint">The hub did not list its machines.</p>
        </div>
      </div>

      <!-- Sessions -->
      <div class="hv-tools">
        <label class="field">
          <Search :size="15" />
          <input v-model="query" placeholder="Filter by title, id, project, machine…" aria-label="Filter hub sessions" />
        </label>
        <label class="checkbox">
          <input v-model="onlyAttention" type="checkbox" />
          <span>Only what needs doing</span>
        </label>
      </div>
      <div v-if="stateChips.length" class="chips hv-chips" role="group" aria-label="Filter by sync state">
        <button
          v-for="c in stateChips"
          :key="c.state"
          class="chip"
          :class="{ on: picked.includes(c.state) }"
          :aria-pressed="picked.includes(c.state)"
          :data-state="c.state"
          :aria-label="`${c.label}, ${c.count}`"
          :title="c.hint"
          @click="toggleState(c.state)"
        >
          <span>{{ c.label }}</span>
          <span class="chip-count">{{ c.count }}</span>
        </button>
      </div>

      <p v-if="!shown.length" class="empty">
        {{ rows.length ? 'Nothing matches these filters.' : 'Nothing on the hub, and nothing here to send.' }}
      </p>

      <table v-else class="hv-table">
        <caption class="sr-only">Sessions on this machine and on the hub</caption>
        <thead>
          <tr>
            <th scope="col"><span class="sr-only">Details</span></th>
            <th scope="col">Sync</th>
            <th scope="col">Session</th>
            <th scope="col" class="hv-col-project">Project</th>
            <th scope="col" class="hv-col-where">Where</th>
            <th scope="col" class="hv-col-when">Updated</th>
            <th scope="col"><span class="sr-only">Action</span></th>
          </tr>
        </thead>
        <tbody>
          <template v-for="r in shown" :key="rowKey(r)">
            <tr class="hv-row" :class="{ open: open.has(rowKey(r)) }">
              <td>
                <button
                  class="icon-btn hv-twist"
                  :aria-expanded="open.has(rowKey(r))"
                  :aria-controls="open.has(rowKey(r)) ? domId(r) : null"
                  :aria-label="`Details for ${r.title || r.short_id}`"
                  @click="toggleRow(r)"
                >
                  <ChevronRight :size="16" />
                </button>
              </td>
              <td>
                <span class="pill hub" :class="[r.state, { stale: hub.stale }]" :title="r.hint">
                  <component :is="iconOf(r)" :size="12" />
                  {{ r.label }}
                </span>
              </td>
              <td>
                <div class="hv-session">
                  <AgentMark :agent="r.agent" :size="15" />
                  <span class="hv-stitle">
                    <span class="truncate" :title="r.title || 'Untitled session'">{{ r.title || 'Untitled session' }}</span>
                    <span class="faint mono">{{ r.short_id }}</span>
                  </span>
                </div>
              </td>
              <td class="hv-col-project" :title="r.project_root">{{ dir(r.project) }}</td>
              <td class="hv-col-where">{{ where(r) }}</td>
              <td class="hv-col-when">{{ when(r.updated) }}</td>
              <td class="hv-act">
                <template v-if="buttonOf(r)">
                  <button
                    class="btn"
                    :aria-disabled="buttonOf(r).off ? 'true' : null"
                    :title="buttonOf(r).off || r.hint"
                    :aria-label="`${buttonOf(r).text} ${r.title || r.short_id}${buttonOf(r).off ? ' (unavailable: ' + buttonOf(r).off + ')' : ''}`"
                    @click="!buttonOf(r).off && press(r)"
                  >
                    <ArrowUp v-if="r.action === 'push'" :size="14" />
                    <ArrowUpDown v-else-if="r.action === 'resolve'" :size="14" />
                    <ArrowDown v-else-if="!buttonOf(r).compare" :size="14" />
                    <Scale v-else :size="14" />
                    <span>{{ buttonOf(r).text }}</span>
                  </button>
                </template>
              </td>
            </tr>
            <tr v-if="open.has(rowKey(r))" :id="domId(r)" class="hv-detail">
              <td />
              <td colspan="6">
                <p class="hv-hint">{{ r.hint }}</p>
                <dl class="hv-dl hv-dl-wide">
                  <dt>Session id</dt>
                  <dd class="mono">{{ r.id }}</dd>
                  <dt>Project</dt>
                  <dd>
                    <span class="mono">{{ dir(r.project_root) }}</span>
                    <span v-if="r.branch" class="faint"> · {{ r.branch }}</span>
                    <span v-if="r.project !== r.project_root" class="faint"> · as {{ r.project }}</span>
                  </dd>
                  <dt>Where it is</dt>
                  <dd>{{ r.here ? 'on this machine' : 'not on this machine' }}, {{ r.on_hub ? 'on the hub' : 'not on the hub' }}</dd>
                  <template v-if="r.on_hub">
                    <dt>Hub copy</dt>
                    <dd>{{ hubCopy(r) }}</dd>
                  </template>
                  <template v-else-if="r.size != null">
                    <dt>Size here</dt>
                    <dd>{{ bytes(r.size) }}</dd>
                  </template>
                  <dt v-if="r.agent_version">Agent version</dt>
                  <dd v-if="r.agent_version">{{ r.agent_version }}</dd>
                  <template v-if="r.compare">
                    <dt>Compared</dt>
                    <dd>
                      {{ bytes(r.compare.local_size) }} here, {{ bytes(r.compare.hub_size) }} on the hub
                      <span class="faint"> — sizes only hint at direction; pulling confirms</span>
                    </dd>
                  </template>
                  <dt>Restorable here</dt>
                  <dd>{{ r.restorable ? 'yes' : 'no — backed up only' }}</dd>
                </dl>
                <p v-if="command(r)" class="hv-cmd">
                  <code class="mono">{{ command(r) }}</code>
                  <button class="icon-btn" :aria-label="`Copy ${command(r)}`" @click="copy(command(r))">
                    <Check v-if="copied === command(r)" :size="14" />
                    <Copy v-else :size="14" />
                  </button>
                  <span class="sr-only" role="status">{{ copied === command(r) ? 'Copied' : '' }}</span>
                </p>
              </td>
            </tr>
          </template>
        </tbody>
      </table>
    </template>
  </section>
</template>
