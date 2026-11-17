<script setup>
import { HBadge } from '@hearth-ui/vue'
import Icon from '../Icon.vue'

// A state badge that never leans on colour alone: an icon and the words.
defineProps({ tone: { type: String, default: 'neutral' }, label: { type: String, required: true }, icon: { type: [String, Object, Function], default: null } })
</script>

<template>
  <HBadge :tone="tone"><span class="sb"><Icon v-if="icon" :icon="icon" :size="14" />{{ label }}</span></HBadge>
</template>

<style>
.sb { display: inline-flex; align-items: center; gap: 4px; white-space: nowrap; }
.sb svg { flex: none; }
</style>
