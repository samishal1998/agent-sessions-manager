<script setup>
import { computed } from 'vue'
import Tooltip from './Tooltip.vue'
import { AGENTS, agentMeta } from '../agents.js'

// An icon rather than a name badge: the list is scanned far more often than
// it is read, and the name is one hover away.
const props = defineProps({
  agent: { type: String, required: true },
  size: { type: Number, default: 17 },
  // No tooltip: for inside a button, where a second focusable would nest.
  plain: { type: Boolean, default: false },
})

const meta = computed(() => agentMeta(props.agent))
// An agent this build has no colour for still gets a mark, just a neutral one.
const cls = computed(() => (props.agent in AGENTS ? props.agent : 'unknown'))
</script>

<template>
  <component :is="plain ? 'span' : Tooltip" v-bind="plain ? {} : { label: meta.label }">
    <span class="agent-mark" :class="cls" :aria-label="meta.label" role="img">
      <component :is="meta.icon" :size="size" :stroke-width="2" />
    </span>
  </component>
</template>
