<script setup>
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import { ArrowLeft, RefreshCw } from 'lucide-vue-next'
import {
  HAlert,
  HBadge,
  HButton,
  HButtonBar,
  HCheckbox,
  HDashboardShell,
  HEmptyState,
  HList,
  HListItem,
  HSkeleton,
  HTheme,
  HThemeSwitcher,
  HToaster,
} from '@hearth-ui/vue'
import api from './api.js'
import { agentOptions, resumeCommand } from './agents.js'
import { confirmDialog, promptDialog } from './dialogs.js'
import { shortId, shortProject } from './ids.js'
import { uniqueTails } from './paths.js'
import { useTheme } from './useTheme.js'
import { dismiss, notify, toasts } from './toasts.js'
import TranscriptView from './TranscriptView.vue'
import AgentMark from './components/AgentMark.vue'
import DialogHost from './components/DialogHost.vue'
import HubPanel from './components/HubPanel.vue'
import HubView from './components/HubView.vue'
import SessionRow from './components/SessionRow.vue'
import SessionToolbar from './components/SessionToolbar.vue'
import './styles/shell.css'

const { preference, resolved, tokens, setPreference } = useTheme()
const sessions = ref([])
const doctor = ref(null)
const filter = ref('')
const fullText = ref('')
const hits = ref(null)
const searchedFor = ref('')
const selectedAgents = ref([])
const selectedSync = ref([])
const projectFilter = ref('')
const showArchived = ref(true)
// Messages go to the toaster, and to a live region for screen readers: a toast
// is a visual extra that comes and goes. `progress` is for the "Doing X…"
// half of an action, which is announced but not worth a toast.
const live = ref('')
const progress = (msg) => (live.value = msg)
const say = (msg, opts) => {
  live.value = msg
  notify(msg, opts)
}
const fail = (msg) => say(msg, { tone: 'danger' })
const scanning = ref(false)
const selected = ref(null)
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
    if (problems.length) fail(`Could not read: ${problems.join('; ')}`)
    projects.value = await api.projects()
  } catch (e) {
    fail(`Could not load sessions: ${e.message}`)
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
    if (stats.index_error) fail(`Indexing stopped: ${stats.index_error}`)
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
// Which of the two screens is showing. The hub screen exists only once this
// machine has joined a hub.
// It lives in the hash (#/sessions, #/hub) so a reload, a link and the back
// button all land on the same screen.
const fromHash = () => (location.hash === '#/hub' ? 'hub' : 'sessions')
const view = ref(fromHash())
const go = (id) => {
  view.value = id
  if (fromHash() !== id) location.hash = `#/${id}`
}
const onHash = () => (view.value = fromHash())
const hubJoined = computed(() => !!hub.value?.joined)
const hubAttention = computed(() => {
  const s = hub.value?.summary
  return s ? s.to_push + s.to_pull + s.to_resolve : 0
})
const navItems = computed(() => [
  { id: 'sessions', label: 'Sessions', icon: 'layout' },
  ...(hubJoined.value
    ? [{ id: 'hub', label: 'Hub', icon: 'globe', badge: hubAttention.value || undefined }]
    : []),
])
// From the hub screen: find the local session a row stands for.
function pushRow(row) {
  const s = sessions.value.find((x) => x.ref.agent === row.agent && x.ref.native_id === row.id)
  if (s) doPush(s)
}
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
  window.addEventListener('hashchange', onHash)
  window.addEventListener('scroll', measureRows, { passive: true })
  window.addEventListener('resize', measureRows)
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
  window.removeEventListener('hashchange', onHash)
  window.removeEventListener('scroll', measureRows)
  window.removeEventListener('resize', measureRows)
})

/* Derived ------------------------------------------------------------- */
// A project is a git repository — every worktree of it, and sessions
// started in subdirectories — so the grouping comes from the server rather
// than from bucketing sessions by their working directory.
const projects = ref([])
const selectedProject = computed(() => projects.value.find((p) => p.root === projectFilter.value))

function inProject(session, project) {
  return project.worktrees.some(
    (w) => session.project_root === w.path || session.project_root.startsWith(`${w.path}/`),
  )
}

// SessionStatus is an internally tagged enum: {"state":"live","pid":N} |
// {"state":"idle"} | {"state":"archived"}.
const statusOf = (s) => s.status?.state ?? s.status

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
// scrolled to, which is the same wait streaming was meant to remove. The page
// itself scrolls (the shell has no inner scroller), so the window follows the
// list's position in the viewport. Below this many, every row is rendered.
const WINDOW_FROM = 150
const ROW_GUESS = 67
const listBox = ref(null)
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
  const box = listBox.value
  if (!box) return
  boxHeight.value = window.innerHeight
  scrolled.value = Math.max(0, -box.getBoundingClientRect().top)
  const row = box.querySelector('.s-row')
  if (!row) return
  const measured = row.getBoundingClientRect().height
  if (measured && Math.abs(measured - rowHeight.value) > 2) rowHeight.value = measured
}

// Opening the transcript, resizing, or filtering changes how tall a row is
// without anyone scrolling.
watch([() => visible.value.length, selected, isNarrow], () => nextTick(measureRows))

// Agents differ in what they support — jcode has no "move", for instance —
// so the row offers only the verbs its agent can actually perform.
const capabilities = computed(() =>
  Object.fromEntries((doctor.value?.agents || []).map((a) => [a.agent, a.capabilities])),
)
// Unknown until doctor answers; assume yes so buttons do not flicker.
const can = (session, verb) => capabilities.value[session.ref.agent]?.[verb] ?? true

const warnings = computed(() =>
  (doctor.value?.agents || []).flatMap((a) => a.warnings.map((w) => `${a.agent}: ${w}`)),
)

/* Presentation -------------------------------------------------------- */
// Where home is, so paths can be shown as `~/…`. Asked once; until it
// arrives paths render in full, which is correct if unlovely.
const home = ref(null)
const hostname = ref(null)
api
  .meta()
  .then((m) => {
    home.value = m.home
    hostname.value = m.hostname
  })
  .catch(() => {})
// Which machine this is: its name on the hub when joined, else its hostname.
const machineName = computed(() => hub.value?.machine || hostname.value)
const shortDir = (root) => shortProject(root, home.value)

// What to call each project: the last segment of its path, grown only as far
// as it takes to tell it from the others.
const projectTails = computed(() => uniqueTails(projects.value.map((p) => shortDir(p.root))))
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
const projectOptions = computed(() =>
  projects.value.map((p) => ({
    value: p.root,
    label: projectLabel(p.root),
    description: [
      projectSubtitle(p.root),
      `${p.session_count} session${p.session_count === 1 ? '' : 's'}`,
      p.worktrees.length > 1 ? `${p.worktrees.length} worktrees` : '',
    ]
      .filter(Boolean)
      .join(' · '),
    keywords: [p.root],
  })),
)

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
const isOpen = (s) => !!selected.value && tickKey(selected.value) === tickKey(s)

function toggleTick(s) {
  const next = new Set(ticked.value)
  next.has(tickKey(s)) ? next.delete(tickKey(s)) : next.add(tickKey(s))
  ticked.value = next
}

// Only what the filters are currently showing — never rows the user
// cannot see.
const allVisibleTicked = computed(() => visible.value.length > 0 && visible.value.every(isTicked))

const toggleAllVisible = () => {
  ticked.value = allVisibleTicked.value ? new Set() : new Set(visible.value.map(tickKey))
}
const clearTicks = () => (ticked.value = new Set())

/// The ticked sessions that are still in the list, resolved now. Anything
/// that disappeared since it was ticked simply is not in the batch.
const tickedSessions = computed(() => sessions.value.filter(isTicked))

const bulkProblems = ref(null)

async function runBulk(action) {
  const batch = tickedSessions.value
  if (!batch.length) return
  progress(`${action.action} ${batch.length} session(s)…`)
  bulkProblems.value = null
  try {
    const report = await api.bulk(batch, action)
    say(report.summary + (report.unresolved?.length ? `; not done: ${report.unresolved.join('; ')}` : ''))
    // Only interrupt when there is something to say beyond the count.
    if (report.problems?.length) bulkProblems.value = report
    clearTicks()
    await refresh()
  } catch (e) {
    fail(`${action.action} failed: ${e.message}`)
  }
}

const bulkArchive = () => runBulk({ action: 'archive' })
const bulkPush = () => runBulk({ action: 'push' }).then(loadHub)
const bulkUnarchive = () => runBulk({ action: 'unarchive' })

async function bulkDelete() {
  const n = tickedSessions.value.length
  const ok = await confirmDialog({
    title: `Delete ${n} sessions?`,
    description: 'Each is backed up first.',
    confirmLabel: 'Delete',
    danger: true,
  })
  if (ok) runBulk({ action: 'delete' })
}

async function bulkMove() {
  const dir = await promptDialog({
    title: `Move ${tickedSessions.value.length} session(s)`,
    label: 'Directory to move them to',
    confirmLabel: 'Move',
  })
  if (dir?.trim()) runBulk({ action: 'move', dir: dir.trim() })
}

async function bulkExport() {
  const dir = await promptDialog({
    title: 'Export sessions',
    description: 'Writes one IR file per session.',
    label: 'Directory to write into',
    confirmLabel: 'Export',
  })
  if (dir?.trim()) runBulk({ action: 'export', dir: dir.trim() })
}

async function bulkImport() {
  // One destination for the batch: whichever agent most of the selection
  // is not already in. jcode is never a destination.
  const claude = tickedSessions.value.filter((s) => s.ref.agent === 'claude-code').length
  const to = claude * 2 > tickedSessions.value.length ? 'opencode' : 'claude-code'
  const name = to === 'opencode' ? 'OpenCode' : 'Claude Code'
  const ok = await confirmDialog({
    title: `Import ${tickedSessions.value.length} session(s) into ${name}?`,
    description: 'Full mode. Already-imported ones are skipped.',
    confirmLabel: 'Import',
  })
  if (ok) runBulk({ action: 'import', to })
}

/* Actions ------------------------------------------------------------- */
// `fn` may return the message to show instead of the stock sentence. What the
// server answers (e.g. `{archived_to: …}`) is data, never the message.
const DONE = {
  Archive: 'Archived.',
  Unarchive: 'Restored from the archive.',
  Rename: 'Renamed.',
  Move: 'Moved.',
  Delete: 'Deleted. A backup was written first.',
  Push: 'Pushed to the hub.',
}
async function act(name, fn) {
  progress(`${name}…`)
  try {
    const msg = await fn()
    say(typeof msg === 'string' && msg ? msg : DONE[name] || `${name} complete.`)
    await refresh()
  } catch (e) {
    fail(`${name} failed: ${e.message}`)
  }
}

async function doRename(s) {
  const title = await promptDialog({ title: 'Rename session', label: 'New title', value: s.title || '', confirmLabel: 'Rename' })
  if (title !== null && title.trim() !== '') act('Rename', () => api.rename(s, title.trim()))
}

function doArchive(s) {
  if (statusOf(s) === 'archived') act('Unarchive', () => api.unarchive(s))
  else act('Archive', () => api.archive(s))
}

// Everything that is new or changed here, in one go.
async function pushNeeded() {
  const batch = sessions.value.filter((s) => hubRow(s)?.action === 'push')
  if (!batch.length) return
  progress(`Pushing ${batch.length}…`)
  try {
    const report = await api.push(batch)
    say(report.summary)
    if (report.problems?.length) bulkProblems.value = report
  } catch (e) {
    fail(`Push failed: ${e.message}`)
  }
  await loadHub()
}

// Everything new or newer on the hub; what cannot land is listed.
async function pullAll() {
  progress('Pulling from the hub…')
  try {
    const report = await api.pullAll()
    say(report.summary)
    if (report.problems?.length) bulkProblems.value = report
  } catch (e) {
    fail(`Pull failed: ${e.message}`)
  }
  await Promise.all([refresh(), loadHub()])
}

function doPush(s) {
  act('Push', async () => {
    const report = await api.push([s])
    // A skip is not a push either: say why, rather than "complete".
    if (report.failed || report.skipped || report.unresolved?.length)
      throw new Error([...report.problems, ...(report.unresolved || [])].join('; '))
    await loadHub()
  })
}

// A pull that has nowhere to go (the pushing machine's path does not exist
// here) asks for a directory once, then tries again with it.
async function doPull(row, projectDir) {
  progress(`Pulling ${row.short_id}…`)
  try {
    const pulled = await api.pull(row.agent, row.id, projectDir)
    const outcome = pulled.outcome?.result
    if (outcome === 'diverged')
      say(`${row.short_id} was continued both here and on ${pulled.from}; nothing changed.`, { tone: 'warning' })
    else say(`Pulled ${row.short_id} into ${shortDir(pulled.project_root)} (${outcome.replace('_', ' ')}).`)
    await Promise.all([refresh(), loadHub()])
  } catch (e) {
    if (!projectDir && e.message.includes('--project-dir')) {
      const dir = await promptDialog({
        title: `Pull ${row.short_id}`,
        description: e.message,
        label: 'Put it in which directory?',
        confirmLabel: 'Pull',
      })
      if (dir?.trim()) return doPull(row, dir.trim())
    }
    fail(`Pull failed: ${e.message}`)
  }
}

// Look at one session against the hub's copy; nothing is installed.
async function doCompare(row) {
  progress(`Comparing ${row.short_id}…`)
  try {
    const c = await api.compare(row.agent, row.id)
    if (c.verdict === 'identical') say(`${row.short_id} is identical on the hub: now synced.`)
    else say(`${row.short_id} differs from the hub; pull it to see which side is ahead.`)
  } catch (e) {
    fail(`Compare failed: ${e.message}`)
  }
  await loadHub()
}

async function doDelete(s) {
  const ok = await confirmDialog({
    title: `Delete ${shortId(s)}?`,
    description: `"${s.title || 'untitled'}". A backup is written first.`,
    confirmLabel: 'Delete',
    danger: true,
  })
  if (ok) act('Delete', () => api.remove(s))
}

async function doMove(s) {
  const dir = await promptDialog({ title: 'Move session', label: 'Project directory', value: s.project_root, confirmLabel: 'Move' })
  if (dir && dir !== s.project_root) act('Move', () => api.move(s, dir))
}

async function doImport(s) {
  // jcode can be an import source but not a destination.
  const to = s.ref.agent === 'claude-code' ? 'opencode' : 'claude-code'
  const name = to === 'opencode' ? 'OpenCode' : 'Claude Code'
  const ok = await confirmDialog({
    title: `Import ${shortId(s)} into ${name}?`,
    description: 'Full mode. Re-importing is a no-op.',
    confirmLabel: 'Import',
  })
  if (!ok) return
  act('Import', async () => {
    const outcome = await api.import(s, to, false)
    return outcome.in_sync
      ? `Already in sync as ${outcome.target?.native_id}.`
      : `Imported as ${outcome.target?.native_id}. ${outcome.resume_hint || ''}`
  })
}

function copyResume(s) {
  const full = `cd ${s.project_root} && ${resumeCommand(s)}`
  navigator.clipboard?.writeText(full)
  say(`Copied: ${full}`)
}

/* Search -------------------------------------------------------------- */
async function runSearch() {
  const query = fullText.value.trim()
  if (!query) return clearSearch()
  progress('Searching…')
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
    live.value = `${hits.value.length} match${hits.value.length === 1 ? '' : 'es'}.`
  } catch (e) {
    fail(`Search failed: ${e.message}`)
  }
}

function clearSearch() {
  hits.value = null
  fullText.value = ''
  searchedFor.value = ''
}

async function reindex() {
  progress('Reindexing…')
  try {
    // It runs on the server's own thread; this follows it.
    await api.indexRefresh()
    await pollIndex()
    if (!indexing.value) say('The index is up to date.')
    else progress('Indexing…')
  } catch (e) {
    fail(`Reindex failed: ${e.message}`)
  }
}

function openHit(hit) {
  const match = sessions.value.find((s) => s.ref.agent === hit.agent && s.ref.native_id === hit.native_id)
  if (match) selected.value = match
  else say('That session is not in the current list — it may be archived.', { tone: 'warning' })
}
</script>

<template>
  <HTheme class="asm-root" theme="dusk" :mode="resolved" :data-asm-mode="resolved" :tokens="tokens">
    <HDashboardShell
      class="asm-shell"
      brand="asm"
      :workspace="machineName || 'This machine'"
      workspace-description="this machine"
      :items="navItems"
      :active="view"
      :page-title="view === 'hub' ? 'Hub' : 'Sessions'"
      :mobile-breakpoint="900"
      @navigate="go"
    >
      <template #sidebar-footer>
        <HThemeSwitcher class="s-theme" :model-value="preference" label="Color mode" @update:model-value="setPreference" />
        <p class="s-side-note">{{ sessions.length }} session{{ sessions.length === 1 ? '' : 's' }} on this machine</p>
      </template>

      <div class="asm-split">
        <div class="asm-main">
          <div v-if="view === 'hub' && hubJoined" class="s-hubwrap">
          <HubView
            :hub="hub"
            :checking="hubChecking"
            :checked-at="hubCheckedAt"
            :home="home"
            @check="loadHub"
            @push="pushRow"
            @pull="doPull"
            @compare="doCompare"
            @push-needed="pushNeeded"
            @pull-all="pullAll"
          />
          </div>

          <div v-show="view === 'sessions' || !hubJoined" class="s-page" :class="{ 'has-drawer': selected }">
            <SessionToolbar
              v-model:filter="filter"
              v-model:full-text="fullText"
              v-model:agents="selectedAgents"
              :project="projectFilter"
              v-model:archived="showArchived"
              :agent-options="AGENT_FILTER_OPTIONS"
              :agent-counts="agentCounts"
              :projects="projectOptions"
              :worktrees="selectedProject && selectedProject.worktrees.length > 1 ? selectedProject.worktrees : []"
              :scanning="scanning"
              :found="sessions.length"
              :indexing="indexing"
              :any-filter="anyFilter"
              @update:project="projectFilter = $event || ''"
              @search="runSearch"
              @clear-search="clearSearch"
              @refresh="refresh"
              @clear="clearFilters"
            >
              <template #chips>
                <HubPanel v-model="selectedSync" mode="chips" :hub="hub" />
              </template>
            </SessionToolbar>

            <HAlert v-for="w in warnings" :key="w" tone="warning" :description="w" />

            <!-- Full-text results -->
            <template v-if="hits !== null">
              <div class="s-results-head">
                <HButton variant="secondary" size="compact" @click="clearSearch">
                  <ArrowLeft :size="15" aria-hidden="true" />
                  <span>Back to sessions</span>
                </HButton>
                <span>
                  {{ hits.length }} match{{ hits.length === 1 ? '' : 'es' }} for
                  <strong>“{{ searchedFor }}”</strong>
                </span>
                <HButton variant="secondary" size="compact" class="s-push-end" @click="reindex">
                  <RefreshCw :size="15" aria-hidden="true" />
                  <span>Reindex</span>
                </HButton>
              </div>

              <HEmptyState v-if="!hits.length" title="No matches" description="Try Reindex if the session is new." />

              <HList v-else label="Search results">
                <HListItem
                  v-for="(h, i) in hits"
                  :key="i"
                  class="s-hit"
                  interactive
                  :title="h.title || 'Untitled session'"
                  @activate="openHit(h)"
                >
                  <template #leading><AgentMark :agent="h.agent" plain /></template>
                  <template #trailing>
                    <HBadge v-if="h.status === 'archived'" tone="accent" label="Archived" />
                    <span class="s-snippet" v-html="highlight(h.snippet)" />
                    <span class="s-hit-seq">#{{ h.seq }}</span>
                  </template>
                </HListItem>
              </HList>
            </template>

            <!-- Session list -->
            <template v-else>
              <HubPanel
                :hub="hub"
                :checking="hubChecking"
                :checked-at="hubCheckedAt"
                @check="loadHub"
                @push-needed="pushNeeded"
                @pull-all="pullAll"
                @open="go('hub')"
              />

              <template v-if="!visible.length">
                <div v-if="scanning" class="s-reading">
                  <HSkeleton variant="text" :lines="4" label="Reading your agent stores" />
                  <p>Reading your agent stores… {{ sessions.length }} found so far.</p>
                </div>
                <HEmptyState
                  v-else-if="!sessions.length"
                  title="No sessions yet"
                  description="No store asm can see has any sessions. Refresh to look again."
                >
                  <HButton variant="secondary" @click="refresh">Refresh</HButton>
                </HEmptyState>
                <HEmptyState v-else title="No sessions match" description="No session fits these filters.">
                  <HButton variant="secondary" @click="clearFilters">Clear filters</HButton>
                </HEmptyState>
              </template>

              <div v-if="visible.length" class="s-bulk">
                <div class="s-bulk-bar">
                  <HCheckbox
                    :label="ticked.size ? `${ticked.size} selected` : 'Select all shown'"
                    :model-value="allVisibleTicked"
                    :indeterminate="ticked.size > 0 && !allVisibleTicked"
                    @change="toggleAllVisible"
                  />
                  <HButtonBar v-if="ticked.size" label="Actions for the selected sessions" align="end">
                    <HButton variant="secondary" size="compact" @click="bulkArchive">Archive</HButton>
                    <HButton variant="secondary" size="compact" @click="bulkUnarchive">Unarchive</HButton>
                    <HButton variant="secondary" size="compact" @click="bulkImport">Import</HButton>
                    <HButton variant="secondary" size="compact" @click="bulkMove">Move</HButton>
                    <HButton variant="secondary" size="compact" @click="bulkExport">Export</HButton>
                    <HButton v-if="hub?.joined" variant="secondary" size="compact" @click="bulkPush">Push</HButton>
                    <HButton variant="danger" size="compact" @click="bulkDelete">Delete</HButton>
                    <HButton variant="ghost" size="compact" @click="clearTicks">Clear</HButton>
                  </HButtonBar>
                </div>
              </div>

              <HAlert v-if="bulkProblems" tone="warning" :title="bulkProblems.summary" dismissible @dismiss="bulkProblems = null">
                <ul class="s-problems">
                  <li v-for="(line, i) in bulkProblems.problems" :key="i">{{ line }}</li>
                </ul>
              </HAlert>

              <div v-if="visible.length" ref="listBox">
                <HList label="Sessions">
                  <li v-if="padTop" :style="{ height: padTop + 'px' }" aria-hidden="true" role="presentation" />
                  <SessionRow
                    v-for="s in shownRows"
                    :key="s.ref.agent + s.ref.native_id"
                    :session="s"
                    :active="isOpen(s)"
                    :ticked="isTicked(s)"
                    :hub-row="hubRow(s)"
                    :hub="hub"
                    :caps="capabilities[s.ref.agent]"
                    :compact="isNarrow || !!selected"
                    :dir="shortDir(s.project_root)"
                    @open="selected = s"
                    @tick="toggleTick(s)"
                    @copy="copyResume(s)"
                    @push="doPush(s)"
                    @pull="doPull(hubRow(s))"
                    @rename="doRename(s)"
                    @archive="doArchive(s)"
                    @move="doMove(s)"
                    @import="doImport(s)"
                    @delete="doDelete(s)"
                  />
                  <li v-if="padBottom" :style="{ height: padBottom + 'px' }" aria-hidden="true" role="presentation" />
                </HList>
              </div>
            </template>
          </div>
        </div>

        <TranscriptView
          v-if="selected"
          :session="selected"
          :modal="isNarrow"
          :can-send="can(selected, 'send_message')"
          @close="selected = null"
        />
      </div>

      <template #footer>
        <span>{{ visible.length }} of {{ sessions.length }} session{{ sessions.length === 1 ? '' : 's' }} shown</span>
      </template>
    </HDashboardShell>

    <!-- Kept mounted so updates are announced as changes, not new content. -->
    <div class="sr-only" role="status" aria-live="polite">{{ live }}</div>
    <HToaster :items="toasts" @dismiss="dismiss" />
    <DialogHost />
  </HTheme>
</template>
