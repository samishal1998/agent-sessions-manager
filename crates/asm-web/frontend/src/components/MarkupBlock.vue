<script setup>
import { computed, ref } from 'vue'
import { HAccordion, HBadge } from '@hearth-ui/vue'
import '../styles/transcript.css'
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

// HAccordion's model is an array of open ids; each block is its own
// one-item accordion, so the id is constant.
const ID = 'b'
function title(node) {
  const attrs = attrPairs(node).map(([k, v]) => (v === '' ? k : `${k}=${v}`))
  const head = [label(node), ...attrs].join(' · ')
  return head
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
    <div v-if="node.type === 'text'" class="tv-text">{{ node.value }}</div>

    <!-- Short leaf: one row, no box. -->
    <div v-else-if="isCompact(node) && depth > 0" class="tv-row">
      <HBadge :label="label(node)" :tone="meta(node).tone === 'error' ? 'danger' : 'neutral'" />
      <span class="tv-row-value">{{ textOf(node) }}</span>
    </div>

    <HAccordion
      v-else
      class="tv-markup"
      :items="[{ id: ID, title: title(node), description: isOpen(node, i) ? undefined : preview(node) }]"
      :model-value="isOpen(node, i) ? [ID] : []"
      @update:model-value="toggle(node, i)"
    >
      <template #[ID]>
        <div v-if="label(node) !== node.name" class="tv-tag">
          <HBadge tone="neutral" :label="node.name" />
        </div>
        <MarkupBlock :nodes="node.children" :depth="depth + 1" />
      </template>
    </HAccordion>
  </template>
</template>
