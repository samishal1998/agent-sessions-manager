<script setup>
import { computed, onMounted, onUnmounted, ref } from 'vue'
import {
  Archive,
  ArchiveRestore,
  ArrowLeft,
  ArrowLeftRight,
  Boxes,
  Download,
  FolderInput,
  GitBranch,
  Menu,
  Pencil,
  Play,
  RefreshCw,
  Search,
  Sparkles,
  SquareTerminal,
  Trash2,
  TriangleAlert,
  Zap,
} from 'lucide-vue-next'
import api from './api.js'
import { shortId, shortProject } from './ids.js'
import TranscriptView from './TranscriptView.vue'
import AgentMark from './components/AgentMark.vue'
import IconButton from './components/IconButton.vue'
import SelectMenu from './components/SelectMenu.vue'
import Tooltip from './components/Tooltip.vue'

const sessions = ref([])
const doctor = ref(null)
const filter = ref('')
const fullText = ref('')
const hits = ref(null)
const searchedFor = ref('')
const selectedAgents = ref([])
const projectFilter = ref('')
const showArchived = ref(true)
const status = ref('')
const selected = ref(null)
const sidebarOpen = ref(false)
// Below this width the transcript covers the list instead of splitting it,
// which also makes it a modal dialog rather than a side panel.
const narrowQuery = window.matchMedia('(max-width: 1100px)')
const isNarrow = ref(narrowQuery.matches)
const onNarrowChange = (e) => (isNarrow.value = e.matches)
let pollTimer = null

const AGENT_OPTIONS = [
  { value: 'claude-code', label: 'Claude Code', icon: Sparkles },
  { value: 'opencode', label: 'OpenCode', icon: SquareTerminal },
  { value: 'jcode', label: 'jcode', icon: Zap },
]

/* Data ---------------------------------------------------------------- */
async function refresh() {
  try {
    const [loaded, grouped] = await Promise.all([api.sessions(false), api.projects()])
    sessions.value = loaded
    projects.value = grouped
  } catch (e) {
    status.value = `Could not load sessions: ${e.message}`
  }
}

async function loadDoctor() {
  try {
    doctor.value = await api.doctor()
  } catch {
    /* non-fatal */
  }
}

onMounted(() => {
  refresh()
  loadDoctor()
  narrowQuery.addEventListener('change', onNarrowChange)
  pollTimer = setInterval(() => {
    if (!document.hidden) refresh()
  }, 5000)
})
onUnmounted(() => {
  clearInterval(pollTimer)
  narrowQuery.removeEventListener('change', onNarrowChange)
})

/* Derived ------------------------------------------------------------- */
// A project is a git repository — every worktree of it, and sessions
// started in subdirectories — so the grouping comes from the server rather
// than from bucketing sessions by their working directory.
const projects = ref([])
const selectedProject = computed(() =>
  projects.value.find((p) => p.root === projectFilter.value),
)

function inProject(session, project) {
  return project.worktrees.some(
    (w) => session.project_root === w.path || session.project_root.startsWith(`${w.path}/`),
  )
}

const visible = computed(() => {
  const needle = filter.value.trim().toLowerCase()
  return sessions.value.filter((s) => {
    if (selectedAgents.value.length && !selectedAgents.value.includes(s.ref.agent)) return false
    if (selectedProject.value && !inProject(s, selectedProject.value)) return false
    if (!showArchived.value && statusOf(s) === 'archived') return false
    if (!needle) return true
    return (
      (s.title || '').toLowerCase().includes(needle) ||
      s.ref.native_id.toLowerCase().includes(needle) ||
      s.project_root.toLowerCase().includes(needle)
    )
  })
})

// Agents differ in what they support — jcode has no "move", for instance —
// so the row offers only the verbs its agent can actually perform.
const capabilities = computed(() =>
  Object.fromEntries((doctor.value?.agents || []).map((a) => [a.agent, a.capabilities])),
)
function can(session, verb) {
  // Unknown until doctor answers; assume yes so buttons do not flicker.
  return capabilities.value[session.ref.agent]?.[verb] ?? true
}

const warnings = computed(() =>
  (doctor.value?.agents || []).flatMap((a) => a.warnings.map((w) => `${a.agent}: ${w}`)),
)

/* Presentation -------------------------------------------------------- */
// Where home is, so paths can be shown as `~/…`. Asked once; until it
// arrives paths render in full, which is correct if unlovely.
const home = ref(null)
api.meta().then((m) => (home.value = m.home)).catch(() => {})
const shortDir = (root) => shortProject(root, home.value)

// SessionStatus is an internally tagged enum: {"state":"live","pid":N} |
// {"state":"idle"} | {"state":"archived"}.
function statusOf(s) {
  return s.status?.state ?? s.status
}

function statusLabel(s) {
  const state = statusOf(s)
  return state === 'live' ? 'Live' : state === 'archived' ? 'Archived' : 'Idle'
}

// The pid is useful but too long for a pill; it belongs in the tooltip.
function statusHint(s) {
  switch (statusOf(s)) {
    case 'live':
      return s.status?.pid
        ? `Running as process ${s.status.pid} — asm will not modify it`
        : 'Running — asm will not modify it'
    case 'archived':
      return 'Archived — restore it before resuming'
    default:
      return 'Not running'
  }
}

// Matches asm_core::fmt::human_bytes so the CLI, TUI and web agree.
function humanBytes(bytes) {
  if (bytes == null) return ''
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  if (unit === 0) return `${bytes} B`
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`
}

function ago(ts) {
  if (!ts) return ''
  const seconds = (Date.now() - new Date(ts).getTime()) / 1000
  if (seconds < 60) return 'just now'
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`
  return `${Math.floor(seconds / 86400)}d ago`
}

// The API marks matched terms with control characters (transcripts contain
// every printable delimiter). Escape the text first, then mark it up —
// snippets are model/user content and must never reach v-html unescaped.
function highlight(snippet) {
  const escaped = snippet
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
  return escaped.replaceAll('\u0001', '<mark>').replaceAll('\u0002', '</mark>')
}

/* Multi-select --------------------------------------------------------
 *
 * Ticks are keyed by agent and id rather than held as object references:
 * every refresh replaces the session objects, and identity comparison
 * would silently empty the selection each time the list reloads.
 */
const ticked = ref(new Set())
const tickKey = (s) => `${s.ref.agent}\u0000${s.ref.native_id}`
const isTicked = (s) => ticked.value.has(tickKey(s))

function toggleTick(s) {
  const next = new Set(ticked.value)
  next.has(tickKey(s)) ? next.delete(tickKey(s)) : next.add(tickKey(s))
  ticked.value = next
}

// Only what the filters are currently showing — never rows the user
// cannot see.
const allVisibleTicked = computed(
  () => visible.value.length > 0 && visible.value.every(isTicked),
)

function toggleAllVisible() {
  ticked.value = allVisibleTicked.value
    ? new Set()
    : new Set(visible.value.map(tickKey))
}

function clearTicks() {
  ticked.value = new Set()
}

/// The ticked sessions that are still in the list, resolved now. Anything
/// that disappeared since it was ticked simply is not in the batch.
const tickedSessions = computed(() => sessions.value.filter(isTicked))

const bulkProblems = ref(null)

async function runBulk(action, confirmText) {
  const batch = tickedSessions.value
  if (!batch.length) return
  if (confirmText && !confirm(confirmText.replace('{n}', batch.length))) return
  status.value = `${action.action} ${batch.length} session(s)…`
  bulkProblems.value = null
  try {
    const report = await api.bulk(batch, action)
    status.value = report.summary
    if (report.unresolved?.length) {
      status.value += `, ${report.unresolved.length} no longer present`
    }
    // Only interrupt when there is something to say beyond the count.
    if (report.problems?.length) bulkProblems.value = report
    clearTicks()
    await refresh()
  } catch (e) {
    status.value = `${action.action} failed: ${e.message}`
  }
}

const bulkArchive = () => runBulk({ action: 'archive' })
const bulkUnarchive = () => runBulk({ action: 'unarchive' })
const bulkDelete = () =>
  runBulk({ action: 'delete' }, 'Delete {n} sessions?\n\nEach is backed up first.')

function bulkMove() {
  const dir = prompt(`Move ${tickedSessions.value.length} session(s) to which directory?`)
  if (dir?.trim()) runBulk({ action: 'move', dir: dir.trim() })
}

function bulkExport() {
  const dir = prompt('Write one IR file per session into which directory?')
  if (dir?.trim()) runBulk({ action: 'export', dir: dir.trim() })
}

function bulkImport() {
  // One destination for the batch: whichever agent most of the selection
  // is not already in. jcode is never a destination.
  const claude = tickedSessions.value.filter((s) => s.ref.agent === 'claude-code').length
  const to = claude * 2 > tickedSessions.value.length ? 'opencode' : 'claude-code'
  const name = to === 'opencode' ? 'OpenCode' : 'Claude Code'
  runBulk(
    { action: 'import', to },
    `Import {n} session(s) into ${name}?\n\nFull mode. Already-imported ones are skipped.`,
  )
}

/* Actions ------------------------------------------------------------- */
async function act(name, fn) {
  status.value = `${name}…`
  try {
    await fn()
    status.value = `${name} complete.`
    await refresh()
  } catch (e) {
    status.value = `${name} failed: ${e.message}`
  }
}

function doRename(s) {
  const title = prompt('New title', s.title || '')
  if (title !== null && title.trim() !== '') act('Rename', () => api.rename(s, title.trim()))
}

function doArchive(s) {
  if (statusOf(s) === 'archived') act('Unarchive', () => api.unarchive(s))
  else act('Archive', () => api.archive(s))
}

function doDelete(s) {
  if (confirm(`Delete ${shortId(s)} "${s.title || 'untitled'}"?\n\nA backup is written first.`))
    act('Delete', () => api.remove(s))
}

function doMove(s) {
  const dir = prompt('Move to project directory', s.project_root)
  if (dir && dir !== s.project_root) act('Move', () => api.move(s, dir))
}

function doImport(s) {
  // jcode can be an import source but not a destination.
  const to = s.ref.agent === 'claude-code' ? 'opencode' : 'claude-code'
  const name = to === 'opencode' ? 'OpenCode' : 'Claude Code'
  if (confirm(`Import ${shortId(s)} into ${name}?\n\nFull mode. Re-importing is a no-op.`))
    act('Import', async () => {
      const outcome = await api.import(s, to, false)
      status.value = outcome.in_sync
        ? `Already in sync as ${outcome.target?.native_id}.`
        : `Imported as ${outcome.target?.native_id}. ${outcome.resume_hint || ''}`
    })
}

const RESUME = {
  'claude-code': (s) => `claude --resume ${s.ref.native_id}`,
  opencode: (s) => `opencode -s ${s.ref.native_id}`,
  // jcode resolves --resume by memorable short name or id.
  jcode: (s) => `jcode --resume ${s.slug || s.ref.native_id}`,
}

function copyResume(s) {
  const cmd = RESUME[s.ref.agent]?.(s) ?? s.ref.native_id
  const full = `cd ${s.project_root} && ${cmd}`
  navigator.clipboard?.writeText(full)
  status.value = `Copied: ${full}`
}

/* Search -------------------------------------------------------------- */
async function runSearch() {
  const query = fullText.value.trim()
  if (!query) return clearSearch()
  status.value = 'Searching…'
  try {
    // The search endpoint narrows to one agent; with none or several
    // selected, ask for everything and narrow the results here.
    const single = selectedAgents.value.length === 1 ? selectedAgents.value[0] : ''
    const found = await api.search(query, single, '')
    hits.value = found.filter(
      (h) =>
        (!selectedAgents.value.length || selectedAgents.value.includes(h.agent)) &&
        // A project spans worktrees, which the single-directory search
        // parameter cannot express — narrow here instead.
        (!selectedProject.value ||
          inProject({ project_root: h.project_root ?? '' }, selectedProject.value)),
    )
    searchedFor.value = query
    status.value = `${hits.value.length} match${hits.value.length === 1 ? '' : 'es'}.`
  } catch (e) {
    status.value = `Search failed: ${e.message}`
  }
}

function clearSearch() {
  hits.value = null
  fullText.value = ''
  searchedFor.value = ''
}

async function reindex() {
  status.value = 'Reindexing…'
  try {
    const report = await api.indexRefresh()
    status.value = `Indexed ${report.reindexed}, unchanged ${report.unchanged}.`
    if (searchedFor.value) {
      fullText.value = searchedFor.value
      await runSearch()
    }
  } catch (e) {
    status.value = `Reindex failed: ${e.message}`
  }
}

function openHit(hit) {
  const match = sessions.value.find(
    (s) => s.ref.agent === hit.agent && s.ref.native_id === hit.native_id,
  )
  if (match) selected.value = match
  else status.value = 'That session is not in the current list — it may be archived.'
}

function pickProject(root) {
  projectFilter.value = projectFilter.value === root ? '' : root
  sidebarOpen.value = false
}
</script>

<template>
  <div class="layout">
    <div v-if="sidebarOpen" class="scrim" @click="sidebarOpen = false" />

    <aside class="sidebar" :class="{ open: sidebarOpen }">
      <div class="brand">
        <Boxes :size="20" />
        <span>asm</span>
      </div>

      <div>
        <div class="side-heading">Projects</div>
        <div class="side-list">
          <button
            class="side-item"
            :class="{ active: projectFilter === '' }"
            @click="pickProject('')"
          >
            <span class="label">All projects</span>
            <span class="count">{{ sessions.length }}</span>
          </button>
          <button
            v-for="p in projects"
            :key="p.root"
            class="side-item"
            :class="{ active: projectFilter === p.root }"
            :title="p.repo ? `${p.root} — ${p.worktrees.length} worktrees` : p.root"
            @click="pickProject(p.root)"
          >
            <GitBranch v-if="p.worktrees.length > 1" :size="13" class="faint" />
            <span class="label">{{ shortDir(p.root) }}</span>
            <span class="count" :title="`${p.session_count} sessions · ${humanBytes(p.size_bytes)}`">
              {{ p.session_count }}
            </span>
          </button>
        </div>
      </div>

      <!-- A project spans checkouts; show them when there is more than one. -->
      <div v-if="selectedProject && selectedProject.worktrees.length > 1">
        <div class="side-heading">Worktrees</div>
        <div class="side-list">
          <div
            v-for="w in selectedProject.worktrees"
            :key="w.path"
            class="side-item"
            :title="w.path"
          >
            <GitBranch v-if="!w.is_main" :size="13" class="faint" />
            <span class="label">{{ w.branch || 'detached' }}</span>
            <span class="count">{{ w.session_count }}</span>
          </div>
        </div>
      </div>

      <div v-if="warnings.length" class="warnings">
        <div v-for="w in warnings" :key="w" class="warning">
          <TriangleAlert :size="15" />
          <span>{{ w }}</span>
        </div>
      </div>
    </aside>

    <main class="main">
      <div class="toolbar">
        <button class="icon-btn mobile-only" aria-label="Show projects" @click="sidebarOpen = true">
          <Menu :size="18" />
        </button>

        <label class="field">
          <Search :size="15" />
          <input v-model="filter" placeholder="Filter by title, ID, or project…" />
        </label>

        <label class="field">
          <Search :size="15" />
          <input
            v-model="fullText"
            placeholder="Search inside transcripts…"
            @keyup.enter="runSearch"
            @keyup.esc="clearSearch"
          />
        </label>

        <SelectMenu
          v-model="selectedAgents"
          :options="AGENT_OPTIONS"
          multiple
          label="Filter by agent"
          placeholder="All agents"
        />

        <label class="checkbox">
          <input v-model="showArchived" type="checkbox" />
          <span>Archived</span>
        </label>

        <button class="btn" @click="refresh">
          <RefreshCw :size="15" />
          <span>Refresh</span>
        </button>
      </div>

      <div class="content">
        <!-- Full-text results -->
        <template v-if="hits !== null">
          <div class="results-head">
            <button class="btn" @click="clearSearch">
              <ArrowLeft :size="15" />
              <span>Back to sessions</span>
            </button>
            <span>
              {{ hits.length }} match{{ hits.length === 1 ? '' : 'es' }} for
              <strong>“{{ searchedFor }}”</strong>
            </span>
            <span class="spacer" />
            <button class="btn" @click="reindex">
              <RefreshCw :size="15" />
              <span>Reindex</span>
            </button>
          </div>

          <div v-if="!hits.length" class="empty">
            No matches. Try Reindex if the session is new.
          </div>

          <div class="rows">
            <div
              v-for="(h, i) in hits"
              :key="i"
              class="row"
              role="button"
              tabindex="0"
              :aria-label="`Open ${h.title || 'untitled session'}, match ${h.seq}`"
              @click="openHit(h)"
              @keydown.enter.prevent="openHit(h)"
              @keydown.space.prevent="openHit(h)"
            >
              <AgentMark :agent="h.agent" />
              <div class="row-body">
                <div class="hit-head">
                  <span class="row-title">{{ h.title || 'Untitled session' }}</span>
                  <span class="pill archived" v-if="h.status === 'archived'">Archived</span>
                </div>
                <div class="hit-snippet" v-html="highlight(h.snippet)" />
              </div>
              <span class="faint mono">#{{ h.seq }}</span>
            </div>
          </div>
        </template>

        <!-- Session list -->
        <template v-else>
          <div v-if="!visible.length" class="empty">No sessions match these filters.</div>

          <div v-if="visible.length" class="bulkbar" :class="{ active: ticked.size > 0 }">
            <label class="tickall">
              <input
                type="checkbox"
                :checked="allVisibleTicked"
                :aria-label="allVisibleTicked ? 'Clear selection' : 'Select all shown'"
                @change="toggleAllVisible"
              />
              <span v-if="ticked.size">{{ ticked.size }} selected</span>
              <span v-else class="faint">Select all shown</span>
            </label>

            <div v-if="ticked.size" class="bulkacts">
              <button class="btn" @click="bulkArchive">Archive</button>
              <button class="btn" @click="bulkUnarchive">Unarchive</button>
              <button class="btn" @click="bulkImport">Import</button>
              <button class="btn" @click="bulkMove">Move</button>
              <button class="btn" @click="bulkExport">Export</button>
              <button class="btn danger" @click="bulkDelete">Delete</button>
              <button class="btn ghost" @click="clearTicks">Clear</button>
            </div>
          </div>

          <div v-if="bulkProblems" class="problems" role="status">
            <div class="problems-head">
              <strong>{{ bulkProblems.summary }}</strong>
              <button class="btn ghost" @click="bulkProblems = null">Dismiss</button>
            </div>
            <ul>
              <li v-for="(line, i) in bulkProblems.problems" :key="i">{{ line }}</li>
            </ul>
          </div>

          <div class="rows">
            <div
              v-for="s in visible"
              :key="s.ref.agent + s.ref.native_id"
              class="row"
              :class="{ selected: selected === s }"
              role="button"
              tabindex="0"
              :aria-current="selected === s"
              :aria-label="`Open transcript of ${s.title || 'untitled session'}`"
              @click="selected = s"
              @keydown.enter.prevent="selected = s"
              @keydown.space.prevent="selected = s"
            >
              <input
                class="tick"
                type="checkbox"
                :checked="isTicked(s)"
                :aria-label="`Select ${s.title || 'untitled session'}`"
                @click.stop="toggleTick(s)"
                @keydown.enter.stop
                @keydown.space.stop
              />

              <AgentMark :agent="s.ref.agent" />

              <div class="row-body">
                <div class="row-title">{{ s.title || 'Untitled session' }}</div>
                <div class="row-meta">
                  <span class="mono">{{ shortId(s) }}</span>
                  <span class="sep">·</span>
                  <span class="project" :title="s.project_root">{{
                    shortDir(s.project_root)
                  }}</span>
                  <span class="sep">·</span>
                  <span>{{ ago(s.updated) }}</span>
                  <template v-if="s.size_bytes != null">
                    <span class="sep">·</span>
                    <span>{{ humanBytes(s.size_bytes) }}</span>
                  </template>
                </div>
              </div>

              <Tooltip :label="statusHint(s)">
                <span class="pill" :class="statusOf(s)">
                  <span v-if="statusOf(s) === 'live'" class="dot" />
                  {{ statusLabel(s) }}
                </span>
              </Tooltip>

              <div class="actions" @click.stop>
                <IconButton label="Copy resume command" :icon="Play" @click="copyResume(s)" />
                <IconButton label="Rename" :icon="Pencil" :disabled="!can(s, 'rename')" @click="doRename(s)" />
                <IconButton
                  :label="statusOf(s) === 'archived' ? 'Restore from archive' : 'Archive'"
                  :icon="statusOf(s) === 'archived' ? ArchiveRestore : Archive"
                  :disabled="!can(s, 'archive')"
                  @click="doArchive(s)"
                />
                <IconButton label="Move to another project" :icon="FolderInput" :disabled="!can(s, 'relocate')" @click="doMove(s)" />
                <IconButton
                  label="Import into the other agent"
                  :icon="ArrowLeftRight"
                  :disabled="!can(s, 'export_ir')"
                  @click="doImport(s)"
                />
                <IconButton
                  label="Export Session IR"
                  :icon="Download"
                  :href="`/api/session/${s.ref.agent}/${s.ref.native_id}/ir`"
                  :download="`${shortId(s)}.ir.json`"
                />
                <IconButton
                  label="Delete"
                  :icon="Trash2"
                  danger
                  :disabled="!can(s, 'delete')"
                  @click="doDelete(s)"
                />
              </div>
            </div>
          </div>
        </template>
      </div>

      <!-- Kept mounted so updates are announced as changes, not new content. -->
      <div class="statusbar" role="status" aria-live="polite">
        {{ status || `${visible.length} session${visible.length === 1 ? '' : 's'}` }}
      </div>
    </main>

    <TranscriptView
      v-if="selected"
      :session="selected"
      :modal="isNarrow"
      @close="selected = null"
    />
  </div>
</template>
