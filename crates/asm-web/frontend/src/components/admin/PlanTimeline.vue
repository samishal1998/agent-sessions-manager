<script setup>
import { computed } from 'vue'
import ResultText from './ResultText.vue'
import StatusBadge from './StatusBadge.vue'
import { STATE_LABEL, STATE_TONE, stateKey } from '../../commands.js'
import { STATE_ICON } from './state-icons.js'

// The steps of one plan, in order: what each does, where, and how it went.
const props = defineProps({ steps: { type: Array, required: true }, label: { type: String, default: 'Steps' }, machines: { type: Object, default: () => ({}) } }) // machines: id -> machine

const byId = computed(() => Object.fromEntries(props.steps.map((c) => [c.id, c])))
const what = (c) => `${c.op === 'push' ? 'Push from' : 'Pull to'} ${c.machine.name}`
const retried = (c) => (c.attempts > 1 ? `${c.attempts} attempts` : '')
</script>

<template>
  <ol class="pt" :aria-label="label">
    <li v-for="c in steps" :key="c.id" class="pt-step">
      <span class="pt-num"><span class="admin-sr">Step </span>{{ c.step }}</span>
      <span class="pt-body">
        <span class="pt-line"><strong class="pt-what">{{ what(c) }}</strong><StatusBadge :tone="STATE_TONE[stateKey(c)]" :label="STATE_LABEL[stateKey(c)]" :icon="STATE_ICON[stateKey(c)]" /></span>
        <ResultText :c="c" :by-id="byId" :machine="machines[c.machine.id]" />
        <span v-if="retried(c)" class="admin-meta">{{ retried(c) }}</span>
      </span>
    </li>
  </ol>
</template>

<style>
.pt { list-style: none; margin: 0; padding: 0; display: grid; gap: 12px; min-width: 0; white-space: normal; }
.pt-step { position: relative; display: grid; grid-template-columns: 24px minmax(0, 1fr); gap: 8px; align-items: start; }
.pt-step:not(:last-child)::before { content: ''; position: absolute; left: 11px; top: 26px; bottom: -10px; width: 2px; background: var(--h-border-strong, var(--h-border, currentColor)); }
.pt-num { box-sizing: border-box; width: 24px; height: 24px; border-radius: 50%; border: 2px solid var(--h-border-strong, currentColor); display: grid; place-items: center; font-size: 12px; font-weight: 700; line-height: 1; background: var(--h-bg, transparent); position: relative; }
.pt-body { display: grid; gap: 4px; justify-items: start; min-width: 0; }
.pt-line { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 8px; min-height: 24px; }
.pt-what { overflow-wrap: anywhere; }
</style>
