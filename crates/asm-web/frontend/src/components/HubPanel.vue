<script setup>
import { HAlert, HBadge, HButton, HChipGroup } from '@hearth-ui/vue'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { ArrowDown, ArrowRight, ArrowUp, RefreshCw } from 'lucide-vue-next'

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
  // 'line' is the one-line status; 'chips' is only the sync-state filter, which
  // the toolbar shows beside the agent chips.
  mode: { type: String, default: 'line' },
})
const emit = defineEmits(['update:modelValue', 'check', 'push-needed', 'pull-all', 'open'])

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
    if (f?.idle) bits.push(`${f.idle} idle`)
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
const options = computed(() =>
  ORDER.map((state) => {
    const of = rows.value.filter((r) => r.state === state)
    return of.length ? { value: state, label: `${of[0].label} (${of.length})` } : null
  }).filter(Boolean),
)
const connection = computed(() => (props.checking ? 'checking' : fresh.value ? 'connected' : 'failed'))
const connectionTone = computed(() => ({ checking: 'info', connected: 'success', failed: 'danger' })[connection.value])
const connectionLabel = computed(() => ({ checking: 'Checking', connected: 'Connected', failed: 'Unreachable' })[connection.value])
const detail = computed(
  () =>
    (props.hub?.machine ? `as ${props.hub.machine}` : '') +
    (ago.value ? `${props.hub?.machine ? ' · ' : ''}${fresh.value ? 'checked' : 'last reached'} ${ago.value}` : ''),
)
const newOnHub = computed(() => rows.value.filter((r) => r.state === 'remote'))
const daemonLabel = computed(() => (daemon.value?.state === 'running' ? 'Daemon' : daemon.value?.state === 'hung' ? 'Daemon stuck' : 'No daemon'))
// What the daemon badge says when hovered: the whole story, one line.
const daemonTitle = computed(() =>
  [daemonLine.value, daemon.value?.state === 'running' ? daemon.value.file?.last_error : daemonHint.value].filter(Boolean).join(' — '),
)
</script>

<template>
  <HChipGroup
    v-if="mode === 'chips' && hub?.joined && options.length"
    :model-value="modelValue"
    label="Filter by sync state"
    :options="options"
    @update:model-value="emit('update:modelValue', $event)"
  />

  <div v-else-if="mode === 'line' && hub?.joined" class="s-hub" role="group" aria-label="Hub">
    <div class="s-hub-line">
      <HBadge :tone="connectionTone" :dot="connection === 'connected'" :label="connectionLabel" :title="detail" />
      <span class="s-hub-host" :title="detail">{{ host }}</span>
      <HBadge
        v-if="daemon"
        class="s-hub-daemon"
        :tone="daemonOk ? 'success' : 'warning'"
        :dot="daemonOk"
        :label="daemonLabel"
        :title="daemonTitle"
      />
      <span class="s-hub-actions">
        <HButton variant="ghost" size="compact" class="s-hub-check" :disabled="checking" aria-label="Check the hub" title="Check the hub" @click="emit('check')">
          <RefreshCw :size="14" aria-hidden="true" />
          <span class="s-hub-check-text">Check</span>
        </HButton>
        <HButton
          v-if="summary.to_push"
          variant="secondary"
          size="compact"
          :disabled="!fresh"
          :title="fresh ? 'Push every session that is new or changed here' : 'The hub cannot be reached'"
          @click="emit('push-needed')"
        >
          <ArrowUp :size="14" aria-hidden="true" />
          <span>Push {{ summary.to_push }}</span>
        </HButton>
        <HButton
          v-if="summary.to_pull"
          variant="secondary"
          size="compact"
          :disabled="!fresh"
          :title="fresh ? 'Pull every session that is new or newer on the hub' : 'The hub cannot be reached'"
          @click="emit('pull-all')"
        >
          <ArrowDown :size="14" aria-hidden="true" />
          <span>Pull {{ summary.to_pull }}</span>
        </HButton>
        <HButton variant="secondary" size="compact" @click="emit('open')">
          <ArrowRight :size="14" aria-hidden="true" />
          <span>Hub view<span v-if="newOnHub.length" class="s-hub-new"> · {{ newOnHub.length }} new</span></span>
        </HButton>
      </span>
    </div>
    <!-- What is wrong, and what to do about it. -->
    <HAlert v-if="hub.error && !checking" tone="warning" :description="hub.error + (hub.stale ? ` Sync states below are as of ${ago}.` : '')" />
  </div>
</template>
