<script setup>
import { nextTick, onUnmounted, ref, watch } from 'vue'
import { HProgress, HSheet } from '@hearth-ui/vue'
import { TriangleAlert } from 'lucide-vue-next'
import StatusBadge from './admin/StatusBadge.vue'
import { STATE_ICON } from './admin/state-icons.js'
import { STATE_TONE } from '../commands.js'
import { isOpen, panel, tasks } from '../tasks.js'

// The server's background tasks, newest first: what each is, how far it got,
// and what it said. A failure, or a report with problems, opens to show them.
const LABEL = { queued: 'Queued', running: 'Running', ok: 'Done', failed: 'Failed' }
const TONE = { ...STATE_TONE, failed: 'danger' }
const ICON = { ...STATE_ICON, failed: TriangleAlert }

// Elapsed time ticks only while the panel is open.
const now = ref(Date.now())
let clock = null
watch(
  () => panel.open,
  (open) => {
    clearInterval(clock)
    if (open) clock = setInterval(() => (now.value = Date.now()), 1000)
  },
  { immediate: true },
)
onUnmounted(() => clearInterval(clock))

function took(t) {
  const s = Math.max(0, Math.round(((t.ended ? Date.parse(t.ended) : now.value) - Date.parse(t.started)) / 1000))
  const spent = s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`
  return t.ended ? `took ${spent}` : `${spent} so far`
}
const problems = (t) => [...(t.result?.problems || []), ...(t.result?.unresolved || [])]
const hasMore = (t) => !!t.error || problems(t).length > 0

// A second click on busy work points at the task already doing it.
const list = ref(null)
watch(
  () => [panel.open, panel.highlight],
  () =>
    nextTick(() => list.value?.querySelector('.tp-row.is-hl')?.scrollIntoView({ block: 'nearest' })),
)
const close = () => Object.assign(panel, { open: false, highlight: null })
</script>

<template>
  <HSheet :open="panel.open" title="Background tasks" description="The last 50 since the server started." width="460px" @close="close">
    <p v-if="!tasks.length" class="tp-faint">Nothing has run yet.</p>
    <ul v-else ref="list" class="tp-list">
      <li v-for="t in tasks" :key="t.id" class="tp-row" :class="{ 'is-hl': t.id === panel.highlight }">
        <div class="tp-head">
          <StatusBadge :tone="TONE[t.state]" :icon="ICON[t.state]" :label="LABEL[t.state] || t.state" />
          <span class="tp-label">{{ t.label }}</span>
          <span class="tp-faint tp-time">{{ took(t) }}</span>
        </div>
        <p v-if="t.state === 'queued'" class="tp-faint">Waits for the task ahead of it to finish.</p>
        <HProgress
          v-else-if="isOpen(t) || t.total"
          :label="t.total ? `${t.done} of ${t.total}` : 'Working'"
          :value="t.done"
          :max="t.total || 1"
          :indeterminate="!t.total"
          :tone="t.state === 'failed' ? 'danger' : 'info'"
        />
        <p v-if="t.result?.summary" class="tp-summary">{{ t.result.summary }}</p>
        <details v-if="hasMore(t)" class="tp-more" :open="t.id === panel.highlight || undefined">
          <summary>{{ t.error ? 'What went wrong' : `${problems(t).length} problem${problems(t).length === 1 ? '' : 's'}` }}</summary>
          <p v-if="t.error" class="tp-error">{{ t.error }}</p>
          <ul v-if="problems(t).length" class="tp-problems">
            <li v-for="(line, i) in problems(t)" :key="i">{{ line }}</li>
          </ul>
        </details>
      </li>
    </ul>
  </HSheet>
</template>

<style>
.tp-list { list-style: none; margin: 0; padding: 0; display: grid; gap: 10px; }
.tp-row { display: grid; gap: 6px; padding: 10px 12px; border: 1px solid var(--h-border); border-radius: 10px; }
.tp-row.is-hl { outline: 2px solid var(--h-accent); outline-offset: 1px; }
.tp-head { display: flex; align-items: center; gap: 8px; min-width: 0; }
.tp-label { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 600; }
.tp-time { flex: none; font-variant-numeric: tabular-nums; }
.tp-faint { color: var(--h-text-muted); font-size: 0.85em; margin: 0; }
.tp-summary, .tp-error { margin: 0; overflow-wrap: anywhere; }
.tp-more summary { cursor: pointer; font-size: 0.9em; }
.tp-problems { margin: 4px 0 0; padding-left: 18px; overflow-wrap: anywhere; }
</style>
