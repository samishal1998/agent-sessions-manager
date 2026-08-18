<script setup>
import { onBeforeUnmount, ref } from 'vue'

// A hover/focus tooltip. The label is also the accessible name of whatever
// it wraps, so icon-only controls are not silent to a screen reader.
//
// It is teleported to <body> and positioned with fixed coordinates rather
// than being absolutely positioned inside the trigger: the session list is
// a scroll container, and an in-flow tooltip on a row flush with its top
// edge gets clipped by that container. Fixed + teleport cannot be clipped,
// and lets it flip below the trigger when there is no room above.
defineProps({ label: { type: String, required: true } })

const shown = ref(false)
const style = ref({})
const wrap = ref(null)

const GAP = 8
const ESTIMATED_HEIGHT = 30

function show() {
  const trigger = wrap.value
  if (!trigger) return
  const rect = trigger.getBoundingClientRect()
  const above = rect.top >= ESTIMATED_HEIGHT + GAP
  style.value = {
    left: `${rect.left + rect.width / 2}px`,
    top: above ? `${rect.top - GAP}px` : `${rect.bottom + GAP}px`,
    transform: above ? 'translate(-50%, -100%)' : 'translate(-50%, 0)',
  }
  shown.value = true
}

function hide() {
  shown.value = false
}

// Scrolling would leave the tooltip stranded next to nothing.
window.addEventListener('scroll', hide, true)
onBeforeUnmount(() => window.removeEventListener('scroll', hide, true))
</script>

<template>
  <span
    ref="wrap"
    class="tt-wrap"
    @mouseenter="show"
    @mouseleave="hide"
    @focusin="show"
    @focusout="hide"
  >
    <slot />
    <Teleport to="body">
      <span v-if="shown" class="tt" role="tooltip" :style="style">{{ label }}</span>
    </Teleport>
  </span>
</template>
