<script setup>
import { computed } from 'vue'
import { HButton } from '@hearth-ui/vue'
import Icon from './Icon.vue'
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
    <Icon :icon="open.length ? 'refresh' : 'list'" :size="14" :class="{ spin: open.length }" />
    <span class="s-tasks-text">{{ text }}</span>
  </HButton>
</template>

<style>
.s-tasks-text { max-width: 28ch; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
