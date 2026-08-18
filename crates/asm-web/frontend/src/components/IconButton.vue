<script setup>
import Tooltip from './Tooltip.vue'

// Icon-only controls need a name for assistive tech and a tooltip for
// everyone else; this keeps the two from drifting apart.
defineProps({
  label: { type: String, required: true },
  icon: { type: [Object, Function], required: true },
  size: { type: Number, default: 16 },
  danger: { type: Boolean, default: false },
  href: { type: String, default: null },
  download: { type: String, default: null },
})
</script>

<template>
  <Tooltip :label="label">
    <a
      v-if="href"
      class="icon-btn"
      :class="{ danger }"
      :href="href"
      :download="download"
      :aria-label="label"
    >
      <component :is="icon" :size="size" />
    </a>
    <button v-else type="button" class="icon-btn" :class="{ danger }" :aria-label="label">
      <component :is="icon" :size="size" />
    </button>
  </Tooltip>
</template>
