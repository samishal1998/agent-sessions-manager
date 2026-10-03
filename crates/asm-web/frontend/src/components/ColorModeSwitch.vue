<script setup>
import { HButton, HTooltip } from '@hearth-ui/vue'
import { Monitor, Moon, Sun } from 'lucide-vue-next'
import { useTheme } from '../useTheme.js'

// Light / dark / follow the system, as three icon buttons in one labelled
// group. Each is a toggle (aria-pressed) with its own name and tooltip, so the
// icons are never the only thing telling a screen reader what they do.
const { preference, setPreference } = useTheme()
const modes = [
  { value: 'light', label: 'Light', icon: Sun },
  { value: 'dark', label: 'Dark', icon: Moon },
  { value: 'system', label: 'Match the system', icon: Monitor },
]
</script>

<template>
  <div class="cm-switch" role="group" aria-label="Color mode">
    <HTooltip v-for="m in modes" :key="m.value" :text="m.label">
      <HButton
        class="cm-btn"
        :class="{ on: preference === m.value }"
        variant="ghost"
        size="compact"
        :aria-label="m.label"
        :aria-pressed="preference === m.value"
        @click="setPreference(m.value)"
      >
        <component :is="m.icon" :size="16" aria-hidden="true" />
      </HButton>
    </HTooltip>
  </div>
</template>

<style>
.cm-switch {
  display: inline-flex;
  gap: 2px;
  padding: 2px;
  border: 1px solid var(--h-border-strong);
  border-radius: var(--h-radius-pill, 999px);
  background: var(--h-surface);
}
.h-button.cm-btn {
  min-height: 30px;
  min-width: 34px;
  padding: 0 8px;
  border-radius: var(--h-radius-pill, 999px);
  color: var(--h-muted);
}
.h-button.cm-btn.on {
  background: var(--h-accent-subtle);
  color: var(--h-accent-text);
  box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--h-accent-text) 45%, transparent);
}
@media (pointer: coarse) {
  .h-button.cm-btn {
    min-height: 40px;
    min-width: 44px;
  }
}
</style>
