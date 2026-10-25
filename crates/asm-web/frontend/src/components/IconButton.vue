<script setup>
import { HButton, HTooltip } from '@hearth-ui/vue'
import { LoaderCircle } from 'lucide-vue-next'

// Icon-only controls need a name for assistive tech and a tooltip for
// everyone else; this keeps the two from drifting apart.
defineProps({
  label: { type: String, required: true },
  icon: { type: [Object, Function], required: true },
  size: { type: Number, default: 16 },
  danger: { type: Boolean, default: false },
  disabled: { type: Boolean, default: false },
  // Its work is under way: a spinner in place of the icon, and no second click.
  busy: { type: Boolean, default: false },
  href: { type: String, default: null },
  download: { type: String, default: null },
})
</script>

<template>
  <HTooltip :text="label">
    <HButton
      class="icon-btn"
      :variant="danger ? 'danger' : 'ghost'"
      size="compact"
      :aria-label="label"
      :disabled="disabled || busy"
      :href="href"
      :download="download"
    >
      <component :is="busy ? LoaderCircle : icon" :size="size" :class="{ spin: busy }" aria-hidden="true" />
    </HButton>
  </HTooltip>
</template>
