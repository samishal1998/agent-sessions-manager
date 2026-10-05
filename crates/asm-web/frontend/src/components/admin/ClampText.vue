<script setup>
import { nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'

// A sentence cut to three lines, with the whole of it behind a "Show more" button when it does not fit
// (an install path in a narrow column would otherwise run down the page).
const props = defineProps({ text: { type: String, required: true }, small: { type: Boolean, default: false }, whole: { type: Boolean, default: false } }) // whole: our own fixed advice, never cut
const el = ref(null)
const open = ref(false)
const over = ref(false)
let watcher
const measure = () => {
  if (el.value && !open.value) over.value = el.value.scrollHeight > el.value.clientHeight + 1
}
onMounted(() => {
  measure()
  if (typeof ResizeObserver !== 'undefined') (watcher = new ResizeObserver(measure)).observe(el.value)
})
onBeforeUnmount(() => watcher?.disconnect())
watch(() => props.text, () => nextTick(measure))
watch(open, () => nextTick(measure))
</script>

<template>
  <span class="cl" :class="{ 'cl-small': small }">
    <span ref="el" class="cl-text" :class="{ 'cl-clamp': !open && !whole }" :title="over && !open ? text : undefined">{{ text }}</span>
    <button v-if="over || open" type="button" class="cl-more" :aria-expanded="open" @click="open = !open">{{ open ? 'Show less' : 'Show more' }}<span class="admin-sr"> of the result</span></button>
  </span>
</template>

<style>
.cl { display: grid; gap: 2px; justify-items: start; min-width: 0; max-width: 100%; white-space: normal; overflow-wrap: anywhere; font-size: 14px; }
.cl-small { font-size: 12px; color: var(--h-text-muted, inherit); }
.cl-clamp { display: -webkit-box; -webkit-line-clamp: 3; -webkit-box-orient: vertical; overflow: hidden; }
.cl-more { all: unset; box-sizing: border-box; cursor: pointer; font-size: 12px; font-weight: 600; color: var(--h-accent-text, inherit); min-height: 24px; line-height: 24px; }
.cl-more:hover { text-decoration: underline; }
.cl-more:focus-visible { outline: 2px solid var(--h-focus, currentColor); outline-offset: 2px; border-radius: 4px; }
</style>
