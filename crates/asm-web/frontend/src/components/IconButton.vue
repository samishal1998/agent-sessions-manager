<script setup>
import { HButton, HTooltip } from '@hearth-ui/vue'
import Icon from './Icon.vue'

// Icon-only controls need a name for assistive tech and a tooltip for
// everyone else; this keeps the two from drifting apart.
defineProps({
  label: { type: String, required: true },
  // A Hearth icon name, or a component for a glyph Hearth does not have.
  icon: { type: [String, Object, Function], required: true },
  size: { type: Number, default: 16 },
  danger: { type: Boolean, default: false },
  disabled: { type: Boolean, default: false },
  // Its work is under way: Hearth's spinner in place of the icon, and no second click.
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
      icon-only
      :icon="typeof icon === 'string' ? icon : undefined"
      :aria-label="label"
      :disabled="disabled"
      :loading="busy"
      :href="href"
      :download="download"
    >
      <template v-if="typeof icon !== 'string'" #icon>
        <Icon :icon="icon" :size="size" />
      </template>
    </HButton>
  </HTooltip>
</template>
