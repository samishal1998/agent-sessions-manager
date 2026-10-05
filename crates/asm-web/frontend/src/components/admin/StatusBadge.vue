<script setup>
import { HBadge } from '@hearth-ui/vue'

// A state badge that never leans on colour alone: an icon and the words.
defineProps({ tone: { type: String, default: 'neutral' }, label: { type: String, required: true }, icon: { type: [Object, Function], default: null } })
</script>

<template>
  <HBadge :tone="tone"><span class="sb"><component :is="icon" v-if="icon" :size="14" aria-hidden="true" />{{ label }}</span></HBadge>
</template>

<style>
.sb { display: inline-flex; align-items: center; gap: 4px; white-space: nowrap; }
.sb svg { flex: none; }
</style>
