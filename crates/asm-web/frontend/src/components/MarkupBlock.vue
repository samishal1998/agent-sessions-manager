<script setup>
import { computed, ref } from 'vue'
import { ChevronDown, ChevronRight } from 'lucide-vue-next'
import { KNOWN, textOf } from '../markup.js'

// Renders the tree from markup.js. Known envelopes get a name and a tone;
// anything else falls back to a collapsible tag tree, which is still far
// more readable than a wall of angle brackets.
const props = defineProps({
  nodes: { type: Array, required: true },
  depth: { type: Number, default: 0 },
})

const openState = ref({})

// Whitespace between elements is layout in the source, not content.
const visible = computed(() =>
  props.nodes.filter((n) => n.type !== 'text' || n.value.trim() !== ''),
)

function meta(node) {
  return KNOWN[node.name] ?? { label: null, tone: 'plain', collapsed: false }
}

/** A short element with no element children reads as one `name: value` row
 *  rather than as another bordered, collapsible box. */
function isCompact(node) {
  if (node.children.some((c) => c.type === 'element')) return false
  return textOf(node).length <= 200
}

function isOpen(node, i) {
  const key = `${i}:${node.name}`
  return openState.value[key] ?? !meta(node).collapsed
}

function toggle(node, i) {
  const key = `${i}:${node.name}`
  openState.value = { ...openState.value, [key]: !isOpen(node, i) }
}

function label(node) {
  return meta(node).label ?? node.name
}

// One line of what is inside, so a collapsed block still says something.
function preview(node) {
  const text = textOf(node).replace(/\s+/g, ' ').trim()
  return text.length > 90 ? `${text.slice(0, 90)}…` : text
}

function attrPairs(node) {
  return Object.entries(node.attrs ?? {})
}
</script>

<template>
  <template v-for="(node, i) in visible" :key="i">
    <div v-if="node.type === 'text'" class="message-text">{{ node.value }}</div>

    <!-- Short leaf: one row, no box. -->
    <div v-else-if="isCompact(node) && depth > 0" class="markup-row">
      <span class="markup-key">{{ label(node) }}</span>
      <span class="markup-value">{{ textOf(node) }}</span>
    </div>

    <div v-else class="markup" :class="[`tone-${meta(node).tone}`, { nested: depth > 0 }]">
      <button
        type="button"
        class="markup-head"
        :aria-expanded="isOpen(node, i)"
        @click="toggle(node, i)"
      >
        <component :is="isOpen(node, i) ? ChevronDown : ChevronRight" :size="13" />
        <span class="markup-label">{{ label(node) }}</span>
        <code v-if="label(node) !== node.name" class="markup-tag">{{ node.name }}</code>
        <code v-for="[k, v] in attrPairs(node)" :key="k" class="markup-attr">
          {{ v === '' ? k : `${k}=${v}` }}
        </code>
        <span v-if="!isOpen(node, i)" class="markup-preview">{{ preview(node) }}</span>
      </button>

      <div v-show="isOpen(node, i)" class="markup-body">
        <MarkupBlock :nodes="node.children" :depth="depth + 1" />
      </div>
    </div>
  </template>
</template>
