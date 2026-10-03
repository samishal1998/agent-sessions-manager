<script setup>
import { computed, onMounted, onUpdated, ref } from 'vue'
import { HBadge, HCheckbox, HDropdownMenu, HListItem } from '@hearth-ui/vue'
import {
  Archive,
  ArchiveRestore,
  ArrowLeftRight,
  CloudDownload,
  CloudUpload,
  Download,
  FolderInput,
  Pencil,
  Play,
  Trash2,
} from 'lucide-vue-next'
import AgentMark from './AgentMark.vue'
import IconButton from './IconButton.vue'
import { ago, bytes, hubTone } from '../format.js'
import { shortId } from '../ids.js'

// One session in the list: a Hearth list item whose leading mark, trailing
// badges and separate actions are filled from Hearth parts. Everything that
// changes anything is an event; the screen owner does the work.
const props = defineProps({
  session: { type: Object, required: true },
  // Open in the transcript panel.
  active: { type: Boolean, default: false },
  ticked: { type: Boolean, default: false },
  // Where the session stands with the hub, if it has been looked up.
  hubRow: { type: Object, default: null },
  hub: { type: Object, default: null },
  // What the session's agent can do, by verb; unknown means yes.
  caps: { type: Object, default: () => ({}) },
  dir: { type: String, default: '' },
  // Secondary actions go behind a menu: on a phone, or with the transcript
  // open beside the list, there is no room for nine buttons.
  compact: { type: Boolean, default: false },
})
const emit = defineEmits(['open', 'tick', 'copy', 'push', 'pull', 'rename', 'archive', 'move', 'import', 'delete'])

const s = computed(() => props.session)
const can = (verb) => props.caps?.[verb] ?? true
// SessionStatus is an internally tagged enum: {"state":"live","pid":N} |
// {"state":"idle"} | {"state":"archived"}.
const state = computed(() => s.value.status?.state ?? s.value.status)
const title = computed(() => s.value.title || 'Untitled session')

const meta = computed(() =>
  [shortId(s.value), ago(s.value.updated), s.value.size_bytes != null ? bytes(s.value.size_bytes) : '', props.dir]
    .filter(Boolean)
    .join(' · '),
)

const statusLabel = computed(() => (state.value === 'live' ? 'Live' : state.value === 'archived' ? 'Archived' : 'Idle'))
const statusTone = computed(() => (state.value === 'live' ? 'success' : state.value === 'archived' ? 'accent' : 'neutral'))
// The pid is useful but too long for a badge; it belongs in the tooltip.
const statusHint = computed(() => {
  if (state.value === 'live')
    return s.value.status?.pid
      ? `Running as process ${s.value.status.pid} — asm will not modify it`
      : 'Running — asm will not modify it'
  return state.value === 'archived' ? 'Archived — restore it before resuming' : 'Not running'
})

// What the push button can do, and the reason when it cannot: pushing a
// session the hub already has, or has a newer copy of, only fails.
const push = computed(() => {
  const r = props.hubRow
  if (props.hub && !props.hub.connected) return { ok: false, label: 'The hub cannot be reached' }
  if (!r) return { ok: true, label: 'Push to hub' }
  if (r.state === 'in_sync') return { ok: false, label: 'Already on the hub' }
  if (r.action === 'pull') return { ok: false, label: 'Pull first: the hub has a newer copy' }
  if (r.state === 'diverged') return { ok: false, label: 'Diverged: resolve with `asm push --force`' }
  return { ok: true, label: r.state === 'local' ? 'Push: not on the hub yet' : 'Push: changed since the last sync' }
})
// HListItem sets its title as text only; the full title is also the native
// tooltip, for a title the row has to truncate.
const item = ref(null)
const tip = () => {
  const el = item.value?.$el?.querySelector('strong')
  if (el) el.title = title.value
}
onMounted(tip)
onUpdated(tip)

const pushing = computed(() => !!props.hub?.joined)
const pulling = computed(() => props.hubRow?.action === 'pull' && !!props.hubRow.restorable)
const pullLabel = computed(() => (props.hub?.connected ? 'Pull from the hub' : 'The hub cannot be reached'))
const archiveLabel = computed(() => (state.value === 'archived' ? 'Restore from archive' : 'Archive'))
const irHref = computed(() => `/api/session/${s.value.ref.agent}/${s.value.ref.native_id}/ir`)

// The same verbs as the buttons, for the menu. Push and pull appear only when
// they could apply; a disabled entry says why in its label.
const menu = computed(() => [
  ...(pushing.value ? [{ id: 'push', label: push.value.label, disabled: !push.value.ok }] : []),
  ...(pulling.value ? [{ id: 'pull', label: pullLabel.value, disabled: !props.hub?.connected }] : []),
  { id: 'rename', label: 'Rename', disabled: !can('rename') },
  { id: 'archive', label: archiveLabel.value, disabled: !can('archive') },
  { id: 'move', label: 'Move to another project', disabled: !can('relocate') },
  { id: 'import', label: 'Import into the other agent', disabled: !can('export_ir') },
  { id: 'export', label: 'Export Session IR' },
  { id: 'delete', label: 'Delete', danger: true, separatorBefore: true, disabled: !can('delete') },
])
function choose(id) {
  if (id !== 'export') return emit(id)
  const a = document.createElement('a')
  a.href = irHref.value
  a.download = `${shortId(s.value)}.ir.json`
  a.click()
}
</script>

<template>
  <HListItem
    ref="item"
    class="s-row"
    interactive
    :title="title"
    :description="meta"
    :selected="active"
    :aria-current="active || null"
    @activate="emit('open')"
  >
    <template #leading><AgentMark :agent="s.ref.agent" plain /></template>

    <template #trailing>
      <span class="s-badges">
        <HBadge :tone="statusTone" :dot="state === 'live'" :label="statusLabel" :title="statusHint" />
        <HBadge
          v-if="hubRow"
          :tone="hubTone(hubRow.state)"
          :class="{ 'is-stale': hub?.stale }"
          :label="hubRow.label"
          :title="hubRow.hint + (hub?.stale ? ' (last known)' : '')"
        />
      </span>
    </template>

    <!-- Actions sit beside the row's button, not inside it. The tick is first
         on screen (order) and right after the button for the keyboard. Every
         slot is always there (a hidden one is an empty placeholder) so an icon
         is in the same place in every row. -->
    <template #actions>
      <HCheckbox
        class="s-tick"
        :label="`Select ${title}`"
        :model-value="ticked"
        @change="emit('tick')"
      />
      <span class="s-actions" :class="{ 'is-compact': compact }">
        <IconButton label="Copy resume command" :icon="Play" @click="emit('copy')" />
        <template v-if="compact">
          <HDropdownMenu class="s-more" :label="`More actions for ${title}`" :items="menu" placement="bottom" @select="choose" />
        </template>
        <template v-else>
          <IconButton v-if="pushing" :label="push.label" :icon="CloudUpload" :disabled="!push.ok" @click="emit('push')" />
          <span v-else class="s-slot" aria-hidden="true" />
          <IconButton v-if="pulling" :label="pullLabel" :disabled="!hub?.connected" :icon="CloudDownload" @click="emit('pull')" />
          <span v-else class="s-slot" aria-hidden="true" />
          <IconButton label="Rename" :icon="Pencil" :disabled="!can('rename')" @click="emit('rename')" />
          <IconButton :label="archiveLabel" :icon="state === 'archived' ? ArchiveRestore : Archive" :disabled="!can('archive')" @click="emit('archive')" />
          <IconButton label="Move to another project" :icon="FolderInput" :disabled="!can('relocate')" @click="emit('move')" />
          <IconButton label="Import into the other agent" :icon="ArrowLeftRight" :disabled="!can('export_ir')" @click="emit('import')" />
          <IconButton label="Export Session IR" :icon="Download" :href="irHref" :download="`${shortId(s)}.ir.json`" />
          <IconButton label="Delete" :icon="Trash2" danger :disabled="!can('delete')" @click="emit('delete')" />
        </template>
      </span>
    </template>
  </HListItem>
</template>
