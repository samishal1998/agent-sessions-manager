<script setup>
import { HIcon } from '@hearth-ui/vue'

// A Hearth icon by name, or a lucide component for a glyph Hearth does not
// have (agents, branches, reasoning…), drawn at Hearth's stroke so the two
// sets sit together. Decorative: the control around it carries the name.
defineProps({
  icon: { type: [String, Object, Function], required: true },
  size: { type: Number, default: 16 },
})
</script>

<template>
  <HIcon v-if="typeof icon === 'string'" :name="icon" :size="size" />
  <component :is="icon" v-else :size="size" :stroke-width="1.8" aria-hidden="true" />
</template>
