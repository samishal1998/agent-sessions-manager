<script setup>
import { computed } from 'vue'
import { HButton } from '@hearth-ui/vue'
import { ListChecks, RefreshCw } from 'lucide-vue-next'
import { isOpen, panel, tasks } from '../tasks.js'

// What the server is doing in the background, in a word; opens the task panel.
const open = computed(() => tasks.value.filter(isOpen))
const text = computed(() =>
  open.value.length > 1 ? `${open.value.length} tasks` : open.value.length ? open.value[0].label : 'Tasks',
)
const aria = computed(() =>
  open.value.length ? `Background tasks: ${open.value.length} under way. Show them` : 'Show recent background tasks',
)
</script>

<template>
  <HButton v-if="tasks.length" class="s-tasks" :variant="open.length ? 'secondary' : 'ghost'" size="compact" :aria-label="aria" @click="panel.open = true">
    <RefreshCw v-if="open.length" :size="13" class="spin" aria-hidden="true" />
    <ListChecks v-else :size="13" aria-hidden="true" />
    <span class="s-tasks-text">{{ text }}</span>
  </HButton>
</template>

<style>
.s-tasks-text { max-width: 28ch; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
