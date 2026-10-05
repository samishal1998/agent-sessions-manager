<script setup>
import { computed } from 'vue'
import { TriangleAlert } from 'lucide-vue-next'
import ClampText from './ClampText.vue'
import { asked, moreDetail, resultText } from '../../commands.js'

// What a command or step says about itself: the result, the hub's own detail,
// and, for a step nobody has picked up, a warning when its machine has been silent.
const props = defineProps({
  c: { type: Object, required: true },
  byId: { type: Object, default: () => ({}) },
  machine: { type: Object, default: null }, // the machine that must run it, from the machines list
})
const text = computed(() => resultText(props.c, props.byId, props.machine))
const more = computed(() => moreDetail(props.c, props.byId))
const silent = computed(() => {
  if (props.c.state !== 'queued' || props.c.cancel_requested) return ''
  const a = asked(props.machine)
  return a?.stale ? `${props.machine.name} ${a.text}: it runs when that machine's daemon is back.` : ''
})
</script>

<template>
  <span class="rt">
    <ClampText v-if="text" :text="text" />
    <ClampText v-if="more" :text="more" small />
    <span v-if="silent" class="rt-warn" role="note"><TriangleAlert :size="14" aria-hidden="true" /><span>{{ silent }}</span></span>
  </span>
</template>

<style>
.rt { display: grid; gap: 4px; justify-items: start; min-width: 0; max-width: 100%; }
.rt-warn { display: flex; gap: 6px; align-items: flex-start; font-size: 12px; white-space: normal; overflow-wrap: anywhere; }
.rt-warn svg { flex: none; margin-top: 1px; color: var(--h-warning, currentColor); }
</style>
