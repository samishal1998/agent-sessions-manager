<script setup>
import { HTooltip } from '@hearth-ui/vue'

// The label is also the tooltip text of whatever it wraps; the wrapped
// control must be focusable and carry its own accessible name.
defineProps({ label: { type: String, required: true } })
</script>

<template>
  <HTooltip :text="label"><slot /></HTooltip>
</template>
