<script setup>
import { computed, onBeforeUnmount, onMounted, nextTick, ref, watch } from 'vue'
import { Check, ChevronDown } from 'lucide-vue-next'

// A listbox that replaces the native <select>: it can multi-select, it can
// be styled, and it still behaves like a menu for the keyboard (arrows to
// move, Enter/Space to toggle, Escape to close, Tab to leave).
const props = defineProps({
  // [{ value, label, icon? }]
  options: { type: Array, required: true },
  // string | string[] depending on `multiple`
  modelValue: { type: [String, Array], required: true },
  multiple: { type: Boolean, default: false },
  // Shown when nothing is selected.
  placeholder: { type: String, default: 'Any' },
  icon: { type: [Object, Function], default: null },
})
const emit = defineEmits(['update:modelValue'])

const open = ref(false)
const activeIndex = ref(-1)
const root = ref(null)
const menu = ref(null)

const selected = computed(() =>
  props.multiple ? props.modelValue : props.modelValue ? [props.modelValue] : [],
)

const summary = computed(() => {
  if (!selected.value.length) return props.placeholder
  if (selected.value.length === 1) {
    return props.options.find((o) => o.value === selected.value[0])?.label ?? selected.value[0]
  }
  return `${selected.value.length} selected`
})

function isSelected(value) {
  return selected.value.includes(value)
}

function toggle(value) {
  if (props.multiple) {
    const next = isSelected(value)
      ? selected.value.filter((v) => v !== value)
      : [...selected.value, value]
    emit('update:modelValue', next)
    // A multi-select stays open: picking two filters should take two
    // clicks, not two open-and-reopen cycles.
  } else {
    emit('update:modelValue', isSelected(value) ? '' : value)
    close()
  }
}

function openMenu() {
  open.value = true
  activeIndex.value = Math.max(
    props.options.findIndex((o) => isSelected(o.value)),
    0,
  )
  nextTick(() => menu.value?.focus())
}

function close() {
  open.value = false
  activeIndex.value = -1
}

function onTriggerKey(event) {
  if (['Enter', ' ', 'ArrowDown'].includes(event.key)) {
    event.preventDefault()
    openMenu()
  }
}

function onMenuKey(event) {
  const last = props.options.length - 1
  switch (event.key) {
    case 'ArrowDown':
      event.preventDefault()
      activeIndex.value = activeIndex.value >= last ? 0 : activeIndex.value + 1
      break
    case 'ArrowUp':
      event.preventDefault()
      activeIndex.value = activeIndex.value <= 0 ? last : activeIndex.value - 1
      break
    case 'Home':
      event.preventDefault()
      activeIndex.value = 0
      break
    case 'End':
      event.preventDefault()
      activeIndex.value = last
      break
    case 'Enter':
    case ' ':
      event.preventDefault()
      if (props.options[activeIndex.value]) toggle(props.options[activeIndex.value].value)
      break
    case 'Escape':
      event.preventDefault()
      close()
      root.value?.querySelector('.select-trigger')?.focus()
      break
    case 'Tab':
      close()
      break
  }
}

function onDocumentPointer(event) {
  if (open.value && root.value && !root.value.contains(event.target)) close()
}

onMounted(() => document.addEventListener('pointerdown', onDocumentPointer))
onBeforeUnmount(() => document.removeEventListener('pointerdown', onDocumentPointer))
watch(() => props.options.length, close)
</script>

<template>
  <div ref="root" class="select">
    <button
      type="button"
      class="select-trigger control"
      :aria-expanded="open"
      aria-haspopup="listbox"
      @click="open ? close() : openMenu()"
      @keydown="onTriggerKey"
    >
      <component :is="icon" v-if="icon" :size="15" />
      <span>{{ summary }}</span>
      <span v-if="multiple && selected.length > 1" class="select-count">{{ selected.length }}</span>
      <ChevronDown class="chev" :size="15" />
    </button>

    <div
      v-if="open"
      ref="menu"
      class="select-menu"
      role="listbox"
      tabindex="-1"
      :aria-multiselectable="multiple"
      @keydown="onMenuKey"
    >
      <button
        v-for="(option, i) in options"
        :key="option.value"
        type="button"
        class="select-option"
        :class="{ active: i === activeIndex }"
        role="option"
        :aria-selected="isSelected(option.value)"
        @click="toggle(option.value)"
        @mousemove="activeIndex = i"
      >
        <Check class="tick" :class="{ hidden: !isSelected(option.value) }" :size="15" />
        <component :is="option.icon" v-if="option.icon" :size="15" />
        <span>{{ option.label }}</span>
      </button>
    </div>
  </div>
</template>
