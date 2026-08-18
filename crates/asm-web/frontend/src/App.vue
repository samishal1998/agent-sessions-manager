<script setup>
import { computed, onMounted, onUnmounted, ref } from 'vue'
import api from './api.js'
import TranscriptView from './TranscriptView.vue'

const sessions = ref([])
const doctor = ref(null)
const filter = ref('')
const agentFilter = ref('')
const projectFilter = ref('')
const showArchived = ref(true)
const status = ref('')
const selected = ref(null)
const fullText = ref('')
const hits = ref(null)
const searchedFor = ref('')
let pollTimer = null

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

async function runSearch() {
  const query = fullText.value.trim()
  if (!query) return clearSearch()
  status.value = 'searching…'
  try {
    hits.value = await api.search(query, agentFilter.value, projectFilter.value)
    searchedFor.value = query
    status.value = `${hits.value.length} match(es)`
  } catch (e) {
    status.value = `search failed: ${e.message}`
  }
}

function clearSearch() {
  hits.value = null
  fullText.value = ''
  searchedFor.value = ''
}

async function reindex() {
  status.value = 'reindexing…'
  try {
    const report = await api.indexRefresh()
    status.value = `indexed ${report.reindexed}, unchanged ${report.unchanged}`
    if (searchedFor.value) {
      fullText.value = searchedFor.value
      await runSearch()
    }
  } catch (e) {
    status.value = `reindex failed: ${e.message}`
  }
}

function openHit(hit) {
  const match = sessions.value.find(
    (s) => s.ref.agent === hit.agent && s.ref.native_id === hit.native_id,
  )
  if (match) selected.value = match
  else status.value = 'that session is no longer in the list (reindex to refresh)'
}

async function refresh() {
  try {
    sessions.value = await api.sessions(false)
  } catch (e) {
    status.value = `error: ${e.message}`
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
  pollTimer = setInterval(() => {
    if (!document.hidden) refresh()
  }, 5000)
})
onUnmounted(() => clearInterval(pollTimer))

const projects = computed(() => {
  const map = new Map()
  for (const s of sessions.value) {
    const key = s.project_root
    map.set(key, (map.get(key) || 0) + 1)
  }
  return [...map.entries()].map(([root, count]) => ({ root, count }))
})

const visible = computed(() => {
  const needle = filter.value.toLowerCase()
  return sessions.value.filter((s) => {
    if (agentFilter.value && s.ref.agent !== agentFilter.value) return false
    if (projectFilter.value && s.project_root !== projectFilter.value) return false
    if (!showArchived.value && statusOf(s) === 'archived') return false
    if (!needle) return true
    return (
      (s.title || '').toLowerCase().includes(needle) ||
      s.ref.native_id.toLowerCase().includes(needle) ||
      s.project_root.toLowerCase().includes(needle)
    )
  })
})

const warnings = computed(() =>
  (doctor.value?.agents || []).flatMap((a) => a.warnings.map((w) => `${a.agent}: ${w}`)),
)

function shortId(s) {
  const id = s.ref.native_id
  return id.startsWith('ses_') ? id.slice(0, 12) : id.slice(0, 8)
}

function shortProject(root) {
  return root.replace(/^\/home\/[^/]+/, '~')
}

// SessionStatus is an internally tagged enum: {"state":"live","pid":N} |
// {"state":"idle"} | {"state":"archived"}.
function statusOf(s) {
  return s.status?.state ?? s.status
}

function statusLabel(s) {
  const state = statusOf(s)
  return state === 'live' && s.status?.pid ? `live (${s.status.pid})` : state
}

function ago(ts) {
  if (!ts) return ''
  const seconds = (Date.now() - new Date(ts).getTime()) / 1000
  if (seconds < 60) return 'now'
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`
  return `${Math.floor(seconds / 86400)}d`
}

async function act(name, fn) {
  status.value = `${name}…`
  try {
    await fn()
    status.value = `${name} done`
    await refresh()
  } catch (e) {
    status.value = `${name} failed: ${e.message}`
  }
}

function doRename(s) {
  const title = prompt('New title', s.title || '')
  if (title !== null && title !== '') act('rename', () => api.rename(s, title))
}

function doArchive(s) {
  if (statusOf(s) === 'archived') act('unarchive', () => api.unarchive(s))
  else act('archive', () => api.archive(s))
}

function doDelete(s) {
  if (confirm(`Delete ${shortId(s)} "${s.title || ''}"? A backup is written first.`))
    act('delete', () => api.remove(s))
}

function doMove(s) {
  const dir = prompt('Move to project directory', s.project_root)
  if (dir) act('move', () => api.move(s, dir))
}

function doImport(s) {
  const to = s.ref.agent === 'claude-code' ? 'opencode' : 'claude-code'
  if (confirm(`Import ${shortId(s)} into ${to}? (full mode, idempotent)`))
    act('import', async () => {
      const outcome = await api.import(s, to, false)
      status.value = outcome.in_sync
        ? `already in sync as ${outcome.target?.native_id}`
        : `imported as ${outcome.target?.native_id} — ${outcome.resume_hint || ''}`
    })
}

function resumeHint(s) {
  const dir = s.project_root
  const cmd =
    s.ref.agent === 'claude-code'
      ? `claude --resume ${s.ref.native_id}`
      : `opencode -s ${s.ref.native_id}`
  navigator.clipboard?.writeText(`cd ${dir} && ${cmd}`)
  status.value = `copied: cd ${dir} && ${cmd}`
}
</script>

<template>
  <div class="layout">
    <aside class="sidebar">
      <h1>asm</h1>
      <div class="side-section">
        <div
          class="side-item"
          :class="{ active: projectFilter === '' }"
          @click="projectFilter = ''"
        >
          all projects <span class="count">{{ sessions.length }}</span>
        </div>
        <div
          v-for="p in projects"
          :key="p.root"
          class="side-item"
          :class="{ active: projectFilter === p.root }"
          :title="p.root"
          @click="projectFilter = projectFilter === p.root ? '' : p.root"
        >
          {{ shortProject(p.root) }} <span class="count">{{ p.count }}</span>
        </div>
      </div>
      <div v-if="warnings.length" class="warnings">
        <div v-for="w in warnings" :key="w">⚠ {{ w }}</div>
      </div>
    </aside>

    <main class="main">
      <div class="toolbar">
        <input v-model="filter" placeholder="filter title / id / project…" class="search" />
        <input
          v-model="fullText"
          placeholder="search inside transcripts… (enter)"
          class="search"
          @keyup.enter="runSearch"
          @keyup.esc="clearSearch"
        />
        <select v-model="agentFilter">
          <option value="">all agents</option>
          <option value="claude-code">claude-code</option>
          <option value="opencode">opencode</option>
        </select>
        <label class="toggle">
          <input v-model="showArchived" type="checkbox" /> archived
        </label>
        <button @click="refresh">refresh</button>
      </div>

      <div v-if="hits !== null" class="results">
        <div class="results-head">
          {{ hits.length }} match(es) for "{{ searchedFor }}"
          <button @click="clearSearch">back to sessions</button>
          <button title="re-scan transcripts into the index" @click="reindex">
            reindex
          </button>
        </div>
        <div
          v-for="(h, i) in hits"
          :key="i"
          class="hit"
          @click="openHit(h)"
        >
          <div class="hit-head">
            <span class="badge" :class="h.agent">{{ h.agent }}</span>
            <strong>{{ h.title || '(untitled)' }}</strong>
            <span class="dim">{{ h.role }} #{{ h.seq }}</span>
            <span v-if="h.status === 'archived'" class="status archived">archived</span>
          </div>
          <div class="hit-snippet" v-html="highlight(h.snippet)"></div>
        </div>
      </div>

      <table v-if="hits === null" class="sessions">
        <thead>
          <tr>
            <th>agent</th>
            <th>id</th>
            <th>title</th>
            <th>project</th>
            <th>updated</th>
            <th>status</th>
            <th class="actions-col">actions</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="s in visible"
            :key="s.ref.agent + s.ref.native_id"
            :class="{ selected: selected === s }"
            @click="selected = s"
          >
            <td><span class="badge" :class="s.ref.agent">{{ s.ref.agent }}</span></td>
            <td class="mono">{{ shortId(s) }}</td>
            <td class="title">{{ s.title || '(untitled)' }}</td>
            <td class="mono dim" :title="s.project_root">{{ shortProject(s.project_root) }}</td>
            <td class="dim">{{ ago(s.updated) }}</td>
            <td>
              <span class="status" :class="statusOf(s)">{{ statusLabel(s) }}</span>
            </td>
            <td class="actions" @click.stop>
              <button title="copy resume command" @click="resumeHint(s)">⏵</button>
              <button title="rename" @click="doRename(s)">✎</button>
              <button title="archive / unarchive" @click="doArchive(s)">🗄</button>
              <button title="move to another project dir" @click="doMove(s)">⇢</button>
              <button title="import into the other agent" @click="doImport(s)">⇄</button>
              <a
                title="export Session IR"
                :href="`/api/session/${s.ref.agent}/${s.ref.native_id}/ir`"
                :download="`${shortId(s)}.ir.json`"
                >⤓</a
              >
              <button class="danger" title="delete (backed up)" @click="doDelete(s)">✕</button>
            </td>
          </tr>
        </tbody>
      </table>
      <div class="statusbar">{{ status || `${visible.length} sessions` }}</div>
    </main>

    <TranscriptView v-if="selected" :session="selected" @close="selected = null" />
  </div>
</template>
