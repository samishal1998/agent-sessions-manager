<script setup>
import { computed } from 'vue'
import { Bot, Sparkles, SquareTerminal, Zap } from 'lucide-vue-next'
import Tooltip from './Tooltip.vue'

// An icon rather than a name badge: the list is scanned far more often than
// it is read, and the name is one hover away.
const props = defineProps({
  agent: { type: String, required: true },
  size: { type: Number, default: 17 },
})

const AGENTS = {
  'claude-code': { icon: Sparkles, name: 'Claude Code' },
  opencode: { icon: SquareTerminal, name: 'OpenCode' },
  jcode: { icon: Zap, name: 'jcode' },
}

const meta = computed(() => AGENTS[props.agent] ?? { icon: Bot, name: props.agent })
const cls = computed(() => (AGENTS[props.agent] ? props.agent : 'unknown'))
</script>

<template>
  <Tooltip :label="meta.name">
    <span class="agent-mark" :class="cls" :aria-label="meta.name" role="img">
      <component :is="meta.icon" :size="size" :stroke-width="2" />
    </span>
  </Tooltip>
</template>
