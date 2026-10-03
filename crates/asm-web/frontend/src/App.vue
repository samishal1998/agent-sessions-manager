<script setup>
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import {
  Archive,
  ArchiveRestore,
  ArrowLeft,
  ArrowLeftRight,
  Boxes,
  CloudDownload,
  CloudUpload,
  Download,
  FilterX,
  FolderInput,
  GitBranch,
  Menu,
  Pencil,
  Play,
  RefreshCw,
  Search,
  Trash2,
  TriangleAlert,
} from 'lucide-vue-next'
import api from './api.js'
import { agentOptions, resumeCommand } from './agents.js'
import { shortId, shortProject } from './ids.js'
import { uniqueTails } from './paths.js'
import TranscriptView from './TranscriptView.vue'
import AgentMark from './components/AgentMark.vue'
import HubPanel from './components/HubPanel.vue'
import IconButton from './components/IconButton.vue'
import Tooltip from './components/Tooltip.vue'

const sessions = ref([])
const doctor = ref(null)
const filter = ref('')
const fullText = ref('')
const hits = ref(null)
const searchedFor = ref('')
const selectedAgents = ref([])
const selectedSync = ref([])
const projectFilter = ref('')
const projectSearch = ref('')
const showArchived = ref(true)
const status = ref('')
const scanning = ref(false)
const selected = ref(null)
const sidebarOpen = ref(false)
// Below this width the transcript covers the list instead of splitting it,
// which also makes it a modal dialog rather than a side panel.
const narrowQuery = window.matchMedia('(max-width: 1100px)')
const isNarrow = ref(narrowQuery.matches)
const onNarrowChange = (e) => (isNarrow.value = e.matches)
let pollTimer = null

// Derived rather than listed. A hand-written table is what let the filter
// go on offering three agents after two more were supported, and nothing in
// the build could catch it. Agents come from what the server reports; their
// names and icons come from ./agents.js, the one place those live.
const AGENT_FILTER_OPTIONS = computed(() =>
  agentOptions([
    ...(doctor.value?.agents || []).map((a) => a.agent),
    // Fall back to the sessions themselves before doctor has answered, so
    // the control is never briefly empty.
    ...sessions.value.map((s) => s.ref.agent),
  ]),
)

// What choosing an agent would leave, with the project, the text and the
// archived switch still applied — not how many exist in total.
const agentCounts = computed(() => {
  const counts = {}
  for (const s of sessions.value) {
    if (matches(s, { ignoreAgents: true })) counts[s.ref.agent] = (counts[s.ref.agent] || 0) + 1
  }
  return counts
})
const toggleAgent = (agent) => {
  selectedAgents.value = selectedAgents.value.includes(agent)
    ? selectedAgents.value.filter((a) => a !== agent)
    : [...selectedAgents.value, agent]
}
const anyFilter = computed(
  () =>
    filter.value !== '' ||
    selectedAgents.value.length > 0 ||
    selectedSync.value.length > 0 ||
    projectFilter.value !== '' ||
    !showArchived.value ||
    hits.value !== null,
)
function clearFilters() {
  filter.value = ''
  selectedAgents.value = []
  selectedSync.value = []
  projectFilter.value = ''
  projectSearch.value = ''
  showArchived.value = true
  clearSearch()
}

/* Data ---------------------------------------------------------------- */
// The first load streams, so rows appear while the stores are still being
// read; the poll after that replaces the list in one go, which keeps it
// from shifting under the pointer.
async function refresh({ streamed = false } = {}) {
  // One scan at a time: the 5s poll must not race the streamed first load
  // and replace a growing list with a shorter one.
  if (scanning.value) return
  try {
    scanning.value = true
    const arriving = []
    let painted = 0
    const problems = await api.sessionsStreamed(false, async (batch) => {
      arriving.push(...batch)
      // The first load fills the list as it goes; a later poll replaces it
      // once, so rows do not shift under the pointer.
      if (!streamed) return
      // Drawing thousands of rows costs more than reading them, so paint a
      // few times a second and give the browser the thread in between —
      // otherwise the list arrives in one go at the end, which is what
      // streaming was meant to avoid.
      const now = performance.now()
      if (now - painted < 150) return
      painted = now
      arriving.sort((a, b) => (b.updated || '').localeCompare(a.updated || ''))
      sessions.value = [...arriving]
      await new Promise((paint) => setTimeout(paint, 0))
    })
    arriving.sort((a, b) => (b.updated || '').localeCompare(a.updated || ''))
    sessions.value = arriving
    if (problems.length) status.value = `Could not read: ${problems.join('; ')}`
    projects.value = await api.projects()
  } catch (e) {
    status.value = `Could not load sessions: ${e.message}`
  } finally {
    scanning.value = false
  }
}

// What the search index is doing, when it is doing anything: on a busy
// machine the first build takes minutes, and searches until then only find
// what it has read so far.
const indexing = ref(null)
let indexTimer = null
async function pollIndex() {
  try {
    const stats = await api.indexStats()
    // `running` covers the stretch before the first session is read, when
    // there is no progress to report but there is work going on.
    indexing.value = stats.indexing || (stats.running ? { done: 0, total: 0 } : null)
    if (stats.index_error) status.value = `Indexing stopped: ${stats.index_error}`
  } catch {
    indexing.value = null
  }
  clearTimeout(indexTimer)
  indexTimer = setTimeout(pollIndex, indexing.value ? 1500 : 15000)
}

// The hub is another machine, so it is asked less often than the local
// stores: on load, every minute, on demand, and after a push or pull. A hub
// that stops answering keeps the last known sync states on screen, marked as
// stale, rather than wiping every pill the moment the network blips.
const hub = ref(null)
const hubChecking = ref(false)
const hubCheckedAt = ref(0)
async function loadHub() {
  hubChecking.value = true
  try {
    const next = await api.hub()
    if (next.connected) hubCheckedAt.value = Date.now()
    else if (hub.value?.rows?.length) {
      next.rows = hub.value.rows
      next.summary = hub.value.summary
      next.stale = true
    }
    hub.value = next
  } catch (e) {
    // This machine's own server did not answer: not the hub's fault, but
    // the same remedy.
    hub.value = { ...(hub.value || {}), joined: true, connected: false, stale: true, error: e.message }
  } finally {
    hubChecking.value = false
  }
}
const hubRows = computed(() =>
  Object.fromEntries((hub.value?.rows || []).map((r) => [`${r.agent}:${r.id}`, r])),
)
const hubRow = (s) => hubRows.value[`${s.ref.agent}:${s.ref.native_id}`]
// Where this session stands with the hub, as the core words it: the label,
// the hint and the next step all come from `RowState` in asm-core.
async function loadDoctor() {
  try {
    doctor.value = await api.doctor()
  } catch {
    /* non-fatal */
  }
}

onMounted(() => {
  refresh({ streamed: true })
  nextTick(measureRows)
  loadDoctor()
  loadHub()
  pollIndex()
  narrowQuery.addEventListener('change', onNarrowChange)
  let ticks = 0
  pollTimer = setInterval(() => {
    if (document.hidden) return
    refresh()
    if (++ticks % 12 === 0) loadHub()
  }, 5000)
})
onUnmounted(() => {
  clearTimeout(indexTimer)
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
const projectSearchShown = computed(() => projects.value.length > 6)
// The sidebar list, narrowed by what was typed into it: matched against
// what each row shows as well as the path, and never hiding the project
// the list is filtered by — that would leave no way back.
const shownProjects = computed(() => {
  const needle = projectSearchShown.value ? projectSearch.value.trim().toLowerCase() : ''
  if (!needle) return projects.value
  return projects.value.filter(
    (p) =>
      p.root === projectFilter.value ||
      p.root.toLowerCase().includes(needle) ||
      projectLabel(p.root).toLowerCase().includes(needle) ||
      (projectSubtitle(p.root) || '').toLowerCase().includes(needle),
  )
})

function inProject(session, project) {
  return project.worktrees.some(
    (w) => session.project_root === w.path || session.project_root.startsWith(`${w.path}/`),
  )
}

function matches(s, { ignoreAgents = false } = {}) {
  const needle = filter.value.trim().toLowerCase()
  if (selectedSync.value.length && !selectedSync.value.includes(hubRow(s)?.state)) return false
  if (!ignoreAgents && selectedAgents.value.length && !selectedAgents.value.includes(s.ref.agent))
    return false
  if (selectedProject.value && !inProject(s, selectedProject.value)) return false
  if (!showArchived.value && statusOf(s) === 'archived') return false
  if (!needle) return true
  return (
    (s.title || '').toLowerCase().includes(needle) ||
    s.ref.native_id.toLowerCase().includes(needle) ||
    s.project_root.toLowerCase().includes(needle)
  )
}

const visible = computed(() => sessions.value.filter((s) => matches(s)))

// Only the rows near the viewport are built. A machine with thousands of
// sessions would otherwise spend seconds constructing rows nobody has
// scrolled to, which is the same wait streaming was meant to remove.
// Below this many, the whole list is rendered and nothing changes.
const WINDOW_FROM = 150
const ROW_GUESS = 67
const contentBox = ref(null)
const scrolled = ref(0)
const boxHeight = ref(900)
const rowHeight = ref(ROW_GUESS)
const windowed = computed(() => visible.value.length > WINDOW_FROM)
const firstRow = computed(() => {
  if (!windowed.value) return 0
  const overscan = 8
  return Math.max(0, Math.floor(scrolled.value / rowHeight.value) - overscan)
})
const shownRows = computed(() => {
  if (!windowed.value) return visible.value
  const fits = Math.ceil(boxHeight.value / rowHeight.value) + 16
  return visible.value.slice(firstRow.value, firstRow.value + fits)
})
const padTop = computed(() => firstRow.value * rowHeight.value)
const padBottom = computed(() =>
  Math.max(0, (visible.value.length - firstRow.value - shownRows.value.length) * rowHeight.value),
)
// The first real row measures the rest: rows are a fixed height at any one
// width, but that height differs between a phone, a desktop, and a desktop
// with the transcript open beside the list.
function measureRows() {
  const box = contentBox.value
  if (!box) return
  boxHeight.value = box.clientHeight
  const row = box.querySelector('.row')
  if (!row) return
  const measured = row.getBoundingClientRect().height + 8
  if (Math.abs(measured - rowHeight.value) > 2) rowHeight.value = measured
}

function onContentScroll(e) {
  scrolled.value = e.target.scrollTop
  measureRows()
}

// Opening the transcript, resizing, or filtering changes how tall a row is
// without anyone scrolling.
watch([() => visible.value.length, selected, isNarrow], () => nextTick(measureRows))

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

/* Sidebar width ------------------------------------------------------- */

const SIDEBAR_MIN = 190
const SIDEBAR_MAX = 560
const SIDEBAR_DEFAULT = 260

// Remembered per browser. Wrapped because a private window, cleared site
// data or a browser set to block storage makes even reading it throw.
function storedWidth() {
  try {
    const saved = Number(localStorage.getItem('asm.sidebarWidth'))
    if (Number.isFinite(saved) && saved >= SIDEBAR_MIN && saved <= SIDEBAR_MAX) return saved
  } catch {
    /* no stored preference is a fine state to be in */
  }
  return SIDEBAR_DEFAULT
}

const sidebarWidth = ref(storedWidth())
const resizing = ref(false)

function setSidebarWidth(px) {
  sidebarWidth.value = Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, Math.round(px)))
}

function persistSidebarWidth() {
  try {
    localStorage.setItem('asm.sidebarWidth', String(sidebarWidth.value))
  } catch {
    /* the width still applies for this page; it just will not be recalled */
  }
}

function startResize(event) {
  // Pointer capture rather than window listeners: the pointer leaves the
  // 5px handle immediately, and without capture the drag stops dead the
  // moment it does.
  event.preventDefault()
  resizing.value = true
  const handle = event.currentTarget
  handle.setPointerCapture?.(event.pointerId)
}

function onResizeMove(event) {
  if (!resizing.value) return
  // Measured from the viewport edge, so the width follows the pointer
  // exactly rather than drifting by wherever the grab started.
  setSidebarWidth(event.clientX)
}

function endResize() {
  if (!resizing.value) return
  resizing.value = false
  persistSidebarWidth()
}

// The keyboard has to be able to do this too; a mouse-only affordance is
// not an affordance for everyone.
function onResizeKey(event) {
  const step = event.shiftKey ? 40 : 10
  const moves = { ArrowLeft: -step, ArrowRight: step }
  if (event.key in moves) {
    event.preventDefault()
    setSidebarWidth(sidebarWidth.value + moves[event.key])
    persistSidebarWidth()
    return
  }
  if (event.key === 'Home' || event.key === 'End') {
    event.preventDefault()
    setSidebarWidth(event.key === 'Home' ? SIDEBAR_MIN : SIDEBAR_MAX)
    persistSidebarWidth()
  }
}

function resetSidebarWidth() {
  setSidebarWidth(SIDEBAR_DEFAULT)
  persistSidebarWidth()
}

/* Project labels ------------------------------------------------------ */

// What to call each project in the sidebar: the last segment of its path,
// grown only as far as it takes to tell it from the others.
const projectTails = computed(() =>
  uniqueTails(projects.value.map((p) => shortDir(p.root))),
)
function projectLabel(root) {
  const shown = shortDir(root)
  return projectTails.value.get(shown) || shown
}
// Suppressed when it would just repeat the label — a one-segment path says
// everything twice otherwise.
function projectSubtitle(root) {
  const shown = shortDir(root)
  return projectLabel(root) === shown ? '' : shown
}

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
      status.value += `; not done: ${report.unresolved.join('; ')}`
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
const bulkPush = () => runBulk({ action: 'push' }).then(loadHub)
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

// What a row's push button can do, and the reason when it cannot: pushing a
// session the hub already has, or has a newer copy of, only fails.
function pushState(s) {
  if (hub.value && !hub.value.connected) return { ok: false, label: 'The hub cannot be reached' }
  const r = hubRow(s)
  if (!r) return { ok: true, label: 'Push to hub' }
  if (r.state === 'in_sync') return { ok: false, label: 'Already on the hub' }
  if (r.action === 'pull') return { ok: false, label: 'Pull first: the hub has a newer copy' }
  if (r.state === 'diverged') return { ok: false, label: 'Diverged: resolve with `asm push --force`' }
  return { ok: true, label: r.state === 'local' ? 'Push: not on the hub yet' : 'Push: changed since the last sync' }
}

// Everything that is new or changed here, in one go.
async function pushNeeded() {
  const batch = sessions.value.filter((s) => hubRow(s)?.action === 'push')
  if (!batch.length) return
  status.value = `Pushing ${batch.length}…`
  try {
    const report = await api.push(batch)
    status.value = report.summary
    if (report.problems?.length) bulkProblems.value = report
  } catch (e) {
    status.value = `Push failed: ${e.message}`
  }
  await loadHub()
}

// Everything new or newer on the hub; what cannot land is listed.
async function pullAll() {
  status.value = 'Pulling from the hub…'
  try {
    const report = await api.pullAll()
    status.value = report.summary
    if (report.problems?.length) bulkProblems.value = report
  } catch (e) {
    status.value = `Pull failed: ${e.message}`
  }
  await Promise.all([refresh(), loadHub()])
}

function doPush(s) {
  act('Push', async () => {
    const report = await api.push([s])
    // A skip is not a push either: say why, rather than "complete".
    if (report.failed || report.skipped || report.unresolved?.length)
      throw new Error(
        [...report.problems, ...(report.unresolved || [])].join('; '),
      )
    await loadHub()
  })
}

// A pull that has nowhere to go (the pushing machine's path does not exist
// here) asks for a directory once, then tries again with it.
async function doPull(row, projectDir) {
  status.value = `Pulling ${row.short_id}…`
  try {
    const pulled = await api.pull(row.agent, row.id, projectDir)
    const outcome = pulled.outcome?.result
    status.value =
      outcome === 'diverged'
        ? `${row.short_id} was continued both here and on ${pulled.from}; nothing changed.`
        : `Pulled ${row.short_id} into ${shortDir(pulled.project_root)} (${outcome.replace('_', ' ')}).`
    await Promise.all([refresh(), loadHub()])
  } catch (e) {
    if (!projectDir && e.message.includes('--project-dir')) {
      const dir = prompt(`${e.message}\n\nPut it in which directory?`)
      if (dir?.trim()) return doPull(row, dir.trim())
    }
    status.value = `Pull failed: ${e.message}`
  }
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

function copyResume(s) {
  const full = `cd ${s.project_root} && ${resumeCommand(s)}`
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
    // It runs on the server's own thread; this follows it.
    await api.indexRefresh()
    await pollIndex()
    if (!status.value.startsWith('Indexing stopped')) {
      status.value = indexing.value ? 'Indexing…' : 'The index is up to date.'
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

    <aside
      class="sidebar"
      :class="{ open: sidebarOpen, resizing }"
      :style="{ '--sidebar-w': sidebarWidth + 'px' }"
    >
      <div class="brand">
        <Boxes :size="20" />
        <span>asm</span>
      </div>

      <div>
        <div class="side-heading">Projects</div>
        <label v-if="projectSearchShown" class="side-search">
          <Search :size="13" />
          <input v-model="projectSearch" placeholder="Find a project…" aria-label="Find a project" />
        </label>
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
            v-for="p in shownProjects"
            :key="p.root"
            class="side-item"
            :class="{ active: projectFilter === p.root }"
            :title="p.repo ? `${p.root} — ${p.worktrees.length} worktrees` : p.root"
            @click="pickProject(p.root)"
          >
            <!-- Always rendered, so a project with worktrees does not sit
                 indented relative to every project without them. -->
            <span class="ico">
              <GitBranch v-if="p.worktrees.length > 1" :size="13" class="faint" />
            </span>
            <span class="stack">
              <span class="label">{{ projectLabel(p.root) }}</span>
              <span v-if="projectSubtitle(p.root)" class="sublabel">{{
                projectSubtitle(p.root)
              }}</span>
            </span>
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

    <div
      class="side-resize"
      role="separator"
      tabindex="0"
      aria-label="Resize the sidebar"
      aria-orientation="vertical"
      :aria-valuenow="sidebarWidth"
      :aria-valuemin="SIDEBAR_MIN"
      :aria-valuemax="SIDEBAR_MAX"
      title="Drag to resize · double-click to reset"
      @pointerdown="startResize"
      @pointermove="onResizeMove"
      @pointerup="endResize"
      @pointercancel="endResize"
      @lostpointercapture="endResize"
      @dblclick="resetSidebarWidth"
      @keydown="onResizeKey"
    />

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

        <label class="checkbox">
          <input v-model="showArchived" type="checkbox" />
          <span>Archived</span>
        </label>

        <span v-if="scanning" class="chip working" title="Reading the agents' stores">
          <span class="spin">⟳</span>
          Reading… {{ sessions.length }}
        </span>

        <span
          v-if="indexing"
          class="chip working"
          :title="`Search reads an index of every transcript; it is being built now${indexing.note ? ' — ' + indexing.note : ''}`"
        >
          <span class="spin">⟳</span>
          <template v-if="indexing.total">Indexing {{ indexing.done }}/{{ indexing.total }}</template>
          <template v-else>Indexing…</template>
        </span>

        <button v-if="anyFilter" class="btn ghost" title="Show every session again" @click="clearFilters">
          <FilterX :size="15" />
          <span>Clear filters</span>
        </button>

        <button class="btn" @click="refresh">
          <RefreshCw :size="15" />
          <span>Refresh</span>
        </button>

        <!-- Five agents at most: one click each beats opening a menu. -->
        <div class="chips" role="group" aria-label="Filter by agent">
          <button
            v-for="a in AGENT_FILTER_OPTIONS"
            :key="a.value"
            class="chip"
            :class="{ on: selectedAgents.includes(a.value) }"
            :aria-pressed="selectedAgents.includes(a.value)"
            :title="selectedAgents.includes(a.value) ? `Stop filtering by ${a.label}` : `Show ${a.label}`"
            @click="toggleAgent(a.value)"
          >
            <component :is="a.icon" :size="14" />
            <span>{{ a.label }}</span>
            <span class="chip-count">{{ agentCounts[a.value] || 0 }}</span>
          </button>
        </div>

      </div>

      <div ref="contentBox" class="content" @scroll.passive="onContentScroll">
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
          <HubPanel
            v-model="selectedSync"
            :hub="hub"
            :checking="hubChecking"
            :checked-at="hubCheckedAt"
            @check="loadHub"
            @push-needed="pushNeeded"
            @pull-all="pullAll"
            @pull="doPull"
          />

          <div v-if="!visible.length" class="empty">
            <template v-if="scanning">
              <span class="spin">⟳</span> Reading your agent stores… {{ sessions.length }} found so far.
            </template>
            <template v-else-if="!sessions.length">
              No sessions in any store asm can see. Refresh to look again.
            </template>
            <template v-else>No sessions match these filters.</template>
          </div>

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
              <button v-if="hub?.joined" class="btn" @click="bulkPush">Push</button>
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
            <div v-if="padTop" :style="{ height: padTop + 'px' }" aria-hidden="true" />
            <div
              v-for="s in shownRows"
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

              <!-- One grid cell for both, so the row keeps its columns. -->
              <div class="pills">
                <Tooltip :label="statusHint(s)">
                  <span class="pill" :class="statusOf(s)">
                    <span v-if="statusOf(s) === 'live'" class="dot" />
                    {{ statusLabel(s) }}
                  </span>
                </Tooltip>
                <Tooltip v-if="hubRow(s)" :label="hubRow(s).hint + (hub?.stale ? ' (last known)' : '')">
                  <span class="pill hub" :class="[hubRow(s).state, { stale: hub?.stale }]">{{
                    hubRow(s).label
                  }}</span>
                </Tooltip>
              </div>

              <div class="actions" @click.stop>
                <IconButton label="Copy resume command" :icon="Play" @click="copyResume(s)" />
                <IconButton
                  v-if="hub?.joined"
                  :label="pushState(s).label"
                  :icon="CloudUpload"
                  :disabled="!pushState(s).ok"
                  @click="doPush(s)"
                />
                <IconButton
                  v-if="hubRow(s)?.action === 'pull' && hubRow(s).restorable"
                  :label="hub?.connected ? 'Pull from the hub' : 'The hub cannot be reached'"
                  :disabled="!hub?.connected"
                  :icon="CloudDownload"
                  @click="doPull(hubRow(s))"
                />
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
            <div v-if="padBottom" :style="{ height: padBottom + 'px' }" aria-hidden="true" />
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
      :can-send="can(selected, 'send_message')"
      @close="selected = null"
    />
  </div>
</template>
