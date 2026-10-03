<script setup>
import { computed } from 'vue'
import { HBadge, HButton, HChipGroup, HCombobox, HInput, HPageHeader, HSwitch, HThemeSwitcher } from '@hearth-ui/vue'
import { useTheme } from '../useTheme.js'
import { FilterX, GitBranch, RefreshCw } from 'lucide-vue-next'

const { preference, setPreference } = useTheme()

// The Sessions page head and its filters. The filter values belong to the
// screen (they decide what the list shows); this only edits them.
const props = defineProps({
  agentOptions: { type: Array, default: () => [] },
  agentCounts: { type: Object, default: () => ({}) },
  // [{ value, label, description, keywords }]
  projects: { type: Array, default: () => [] },
  // Checkouts of the chosen project, when it has more than one.
  worktrees: { type: Array, default: () => [] },
  scanning: { type: Boolean, default: false },
  found: { type: Number, default: 0 },
  indexing: { type: Object, default: null },
  anyFilter: { type: Boolean, default: false },
})
const emit = defineEmits(['search', 'clear-search', 'refresh', 'clear'])

const filter = defineModel('filter', { type: String, default: '' })
const fullText = defineModel('fullText', { type: String, default: '' })
const agents = defineModel('agents', { type: Array, default: () => [] })
const project = defineModel('project', { type: String, default: '' })
const archived = defineModel('archived', { type: Boolean, default: true })

// Counts go in the label: HChipGroup has no slot for them.
const chipOptions = computed(() =>
  props.agentOptions.map((a) => ({ value: a.value, label: `${a.label} (${props.agentCounts[a.value] || 0})` })),
)
</script>

<template>
  <div class="s-toolbar">
    <HPageHeader class="s-head" title="Sessions">
      <!-- The shell's mobile drawer has no footer, so on phones the colour mode lives here. -->
      <HThemeSwitcher class="s-theme-mobile" :model-value="preference" label="Color mode" @update:model-value="setPreference" />
      <HSwitch v-model="archived" label="Archived" />
      <HButton variant="secondary" size="compact" @click="emit('refresh')">
        <RefreshCw :size="15" aria-hidden="true" />
        <span>Refresh</span>
      </HButton>
    </HPageHeader>

    <div class="s-fields">
      <HInput v-model="filter" type="search" label="Filter sessions" placeholder="Title, ID, or project" />
      <!-- Enter runs the search, Escape drops it. -->
      <div class="s-field-wrap" @keyup.enter="emit('search')" @keyup.esc="emit('clear-search')">
        <HInput
          v-model="fullText"
          type="search"
          label="Search inside transcripts"
          placeholder="Search transcripts, then Enter"
        />
      </div>
      <HCombobox
        v-model="project"
        label="Project"
        placeholder="All projects"
        empty-text="No project matches"
        clearable
        :options="projects"
      />
    </div>

    <!-- Agent chips, then whatever the screen adds (sync states), in one wrapping row. -->
    <div class="s-chips">
      <HChipGroup v-model="agents" label="Filter by agent" :options="chipOptions" />
      <slot name="chips" />
    </div>

    <div v-if="scanning || indexing || anyFilter || worktrees.length" class="s-status">
      <HBadge v-if="scanning" tone="info" title="Reading the agents' stores">
        <RefreshCw :size="12" class="spin" aria-hidden="true" /> Reading… {{ found }}
      </HBadge>
      <HBadge
        v-if="indexing"
        tone="info"
        :title="`Search reads an index of every transcript; it is being built now${indexing.note ? ' — ' + indexing.note : ''}`"
      >
        <RefreshCw :size="12" class="spin" aria-hidden="true" />
        <template v-if="indexing.total"> Indexing {{ indexing.done }}/{{ indexing.total }}</template>
        <template v-else> Indexing…</template>
      </HBadge>
      <!-- A project spans checkouts; name them when there is more than one. -->
      <HBadge v-for="w in worktrees" :key="w.path" tone="neutral" :title="w.path">
        <GitBranch v-if="!w.is_main" :size="12" aria-hidden="true" /> {{ w.branch || 'detached' }} · {{ w.session_count }}
      </HBadge>
      <HButton v-if="anyFilter" variant="ghost" size="compact" title="Show every session again" @click="emit('clear')">
        <FilterX :size="15" aria-hidden="true" />
        <span>Clear filters</span>
      </HButton>
    </div>
  </div>
</template>
