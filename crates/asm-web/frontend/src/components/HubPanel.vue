<script setup>
import { computed, onMounted, onUnmounted, ref } from 'vue'
import {
  ArrowDown,
  ArrowUp,
  ArrowUpDown,
  Check,
  Cloud,
  CloudDownload,
  CloudOff,
  Radio,
  RefreshCw,
  TriangleAlert,
} from 'lucide-vue-next'

// How this machine stands with the hub, in one place: whether it answered,
// what is out of step, and what is on the hub that is not here yet. The
// wording and the next step for each state come from the core
// (`RowState::label/hint/action`) so this, the TUI and the CLI cannot drift.
const props = defineProps({
  hub: { type: Object, default: null },
  checking: { type: Boolean, default: false },
  // When the hub last answered, as ms since the epoch.
  checkedAt: { type: Number, default: 0 },
  // Sync states the list is narrowed to.
  modelValue: { type: Array, default: () => [] },
})
const emit = defineEmits(['update:modelValue', 'check', 'push-needed', 'pull-all', 'pull'])

const now = ref(Date.now())
let clock = null
onMounted(() => (clock = setInterval(() => (now.value = Date.now()), 15000)))
onUnmounted(() => clearInterval(clock))

const ago = computed(() => {
  if (!props.checkedAt) return ''
  const s = Math.max(0, Math.round((now.value - props.checkedAt) / 1000))
  if (s < 10) return 'just now'
  if (s < 90) return `${s}s ago`
  return `${Math.round(s / 60)}m ago`
})

const host = computed(() => {
  try {
    return new URL(props.hub.url).host
  } catch {
    return props.hub?.url || 'the hub'
  }
})

// The background `asm daemon` on this machine, as it last reported. Times
// are unix seconds from the file; `now` ticks so "12s ago" does not freeze.
const daemon = computed(() => props.hub?.daemon || null)
const since = (t) => {
  if (!t) return 'not yet'
  const s = Math.max(0, Math.round(now.value / 1000 - t))
  if (s < 60) return `${s}s ago`
  if (s < 3600) return `${Math.round(s / 60)}m ago`
  return `${Math.round(s / 3600)}h ago`
}
const daemonLine = computed(() => {
  const d = daemon.value
  if (!d) return ''
  const f = d.file
  if (d.state === 'running') {
    const bits = [`Daemon running · last push ${since(f?.last_push)}`]
    if (f?.pending) bits.push(`${f.pending} waiting`)
    return bits.join(' · ')
  }
  if (d.state === 'hung') return `Daemon not responding — no pass finished for ${since(f?.last_pass || f?.started).replace(' ago', '')}`
  return f ? `Daemon stopped (last ran ${since(f.last_pass || f.started)})` : 'No daemon on this machine'
})
const daemonHint = computed(() =>
  daemon.value?.state === 'running'
    ? daemon.value.file?.last_error || ''
    : 'Run `asm daemon` to keep the hub up to date automatically.',
)
const daemonOk = computed(() => daemon.value?.state === 'running' && !daemon.value.file?.last_error)

const rows = computed(() => props.hub?.rows || [])
const summary = computed(() => props.hub?.summary || {})
const fresh = computed(() => props.hub?.connected && !props.hub?.stale)

// One chip per state that has sessions in it, in the order a person works
// through them: what to send, what to fetch, what needs a decision, the rest.
const ORDER = ['local', 'ahead', 'behind', 'untracked', 'diverged', 'in_sync']
const ICONS = { local: ArrowUp, ahead: ArrowUp, behind: ArrowDown, untracked: ArrowDown, diverged: ArrowUpDown, in_sync: Check }
const chips = computed(() =>
  ORDER.map((state) => {
    const of = rows.value.filter((r) => r.state === state)
    return of.length ? { state, label: of[0].label, hint: of[0].hint, count: of.length } : null
  }).filter(Boolean),
)
const newOnHub = computed(() => rows.value.filter((r) => r.state === 'remote'))
// A hub with a hundred sessions this machine lacks would bury the list, so
// the first few show and the rest are one click away.
const SHOWN = 4
const expanded = ref(false)
const newShown = computed(() => (expanded.value ? newOnHub.value : newOnHub.value.slice(0, SHOWN)))

function toggle(state) {
  emit(
    'update:modelValue',
    props.modelValue.includes(state)
      ? props.modelValue.filter((s) => s !== state)
      : [...props.modelValue, state],
  )
}
</script>

<template>
  <section v-if="hub?.joined" class="hubpanel" aria-label="Hub">
    <div class="hub-line">
      <!-- Icon and words, never colour alone; announced when it changes. -->
      <span
        class="hub-state"
        :class="fresh ? 'ok' : checking ? 'wait' : 'bad'"
        role="status"
        aria-live="polite"
      >
        <RefreshCw v-if="checking" :size="15" class="spin" />
        <Cloud v-else-if="fresh" :size="15" />
        <CloudOff v-else :size="15" />
        <template v-if="checking">Checking the hub…</template>
        <template v-else-if="fresh">Connected to {{ host }}</template>
        <template v-else>Can't reach the hub</template>
      </span>
      <span class="faint hub-detail">
        <template v-if="hub.machine">as {{ hub.machine }}</template>
        <template v-if="ago"> · {{ fresh ? 'checked' : 'last reached' }} {{ ago }}</template>
      </span>

      <span class="hub-actions">
        <button class="btn" :disabled="checking" @click="emit('check')">
          <RefreshCw :size="14" />
          <span>{{ fresh ? 'Check' : 'Retry' }}</span>
        </button>
        <button
          v-if="summary.to_push"
          class="btn"
          :disabled="!fresh"
          :title="fresh ? 'Push every session that is new or changed here' : 'The hub cannot be reached'"
          @click="emit('push-needed')"
        >
          <ArrowUp :size="14" />
          <span>Push {{ summary.to_push }}</span>
        </button>
        <button
          v-if="summary.to_pull"
          class="btn"
          :disabled="!fresh"
          :title="fresh ? 'Pull every session that is new or newer on the hub' : 'The hub cannot be reached'"
          @click="emit('pull-all')"
        >
          <ArrowDown :size="14" />
          <span>Pull {{ summary.to_pull }}</span>
        </button>
      </span>
    </div>

    <div v-if="daemon" class="hub-daemon" :class="{ ok: daemonOk }" :title="daemonHint">
      <Radio :size="14" />
      <span>{{ daemonLine }}</span>
      <span v-if="daemon.state === 'running' && daemon.file?.last_error" class="hub-daemon-err">
        — {{ daemon.file.last_error }}
      </span>
      <span v-else-if="daemon.state !== 'running'" class="faint">— {{ daemonHint }}</span>
    </div>

    <!-- What is wrong, and what to do about it. -->
    <div v-if="hub.error && !checking" class="hub-problem" role="alert" :title="hub.detail">
      <TriangleAlert :size="15" />
      <span>
        {{ hub.error }}
        <template v-if="hub.stale"> Sync states below are as of {{ ago }}.</template>
      </span>
    </div>

    <div v-if="chips.length" class="chips" role="group" aria-label="Filter by sync state">
      <button
        v-for="c in chips"
        :key="c.state"
        class="chip"
        :class="{ on: modelValue.includes(c.state) }"
        :aria-pressed="modelValue.includes(c.state)"
        :title="c.hint"
        @click="toggle(c.state)"
      >
        <component :is="ICONS[c.state]" :size="14" />
        <span>{{ c.label }}</span>
        <span class="chip-count">{{ c.count }}</span>
      </button>
    </div>

    <div v-if="newOnHub.length" class="hub-new">
      <div class="hub-new-head">
        <CloudDownload :size="15" />
        <strong>New on the hub</strong>
        <span class="faint">{{ newOnHub.length }} not on this machine</span>
      </div>
      <ul>
        <li v-for="r in newShown" :key="r.agent + r.id">
          <span class="hub-new-title" :title="r.title || r.short_id">{{ r.title || r.short_id }}</span>
          <span class="faint hub-new-meta">{{ r.machine }} · {{ r.agent }}</span>
          <button
            class="btn"
            :disabled="!r.restorable || !fresh"
            :title="
              !fresh
                ? 'The hub cannot be reached'
                : r.restorable
                  ? `Pull ${r.title || r.short_id} from ${r.machine}`
                  : `${r.agent} sessions are backed up on the hub but cannot be restored here yet`
            "
            :aria-label="`Pull ${r.title || r.short_id} from ${r.machine}`"
            @click="emit('pull', r)"
          >
            <ArrowDown :size="14" />
            <span>Pull</span>
          </button>
        </li>
      </ul>
      <button
        v-if="newOnHub.length > SHOWN"
        class="btn ghost"
        :aria-expanded="expanded"
        @click="expanded = !expanded"
      >
        {{ expanded ? 'Show fewer' : `Show all ${newOnHub.length}` }}
      </button>
    </div>
  </section>
</template>
