<script setup>
import { computed, onBeforeUnmount, onMounted, nextTick, ref, watch } from 'vue'
import {
  Brain,
  ChevronDown,
  ChevronRight,
  CornerDownRight,
  Paperclip,
  Search,
  Wrench,
  X,
} from 'lucide-vue-next'
import api from './api.js'
import AgentMark from './components/AgentMark.vue'
import IconButton from './components/IconButton.vue'

const props = defineProps({
  session: { type: Object, required: true },
  // When the drawer covers the page rather than sitting beside it, it is a
  // modal dialog and has to say so.
  modal: { type: Boolean, default: false },
})
const emit = defineEmits(['close'])

const root = ref(null)
// Opening the drawer must move focus into it, or the keyboard is left
// behind on the page underneath; closing must hand focus back.
let previouslyFocused = null
onMounted(() => {
  previouslyFocused = document.activeElement
  nextTick(() => root.value?.focus())
})
onBeforeUnmount(() => {
  if (previouslyFocused?.isConnected) previouslyFocused.focus()
})

const ir = ref(null)
const error = ref('')
const search = ref('')
const windowSize = ref(150)
const expanded = ref(new Set())

watch(
  () => props.session,
  async (session) => {
    ir.value = null
    error.value = ''
    windowSize.value = 150
    expanded.value = new Set()
    try {
      ir.value = await api.ir(session)
    } catch (e) {
      error.value = e.message
    }
  },
  { immediate: true },
)

const matching = computed(() => {
  if (!ir.value) return []
  const needle = search.value.trim().toLowerCase()
  if (!needle) return ir.value.messages
  return ir.value.messages.filter((m) =>
    m.parts.some((p) => {
      const text = p.text || p.summary || p.output || JSON.stringify(p.input || '')
      return text.toLowerCase().includes(needle)
    }),
  )
})

const visible = computed(() => matching.value.slice(-windowSize.value))
const hidden = computed(() => Math.max(matching.value.length - windowSize.value, 0))

function toggle(key) {
  const set = new Set(expanded.value)
  set.has(key) ? set.delete(key) : set.add(key)
  expanded.value = set
}

// First line with something on it: a leading blank line would otherwise
// render as an empty row with only an icon in it.
function firstLine(text) {
  const line = (text || '').split('\n').find((l) => l.trim()) ?? ''
  return line.length > 160 ? `${line.slice(0, 160)}…` : line
}

// Claude stores signed thinking blocks whose readable text can be empty.
// The part still means "the model reasoned here", so say that rather than
// showing a lone icon.
function reasoningText(part) {
  return firstLine(part.summary) || 'Reasoning was not recorded in the transcript'
}

function inputSummary(input) {
  if (!input || typeof input !== 'object') return ''
  for (const key of ['command', 'file_path', 'path', 'pattern', 'query', 'url']) {
    if (typeof input[key] === 'string') return input[key]
  }
  return firstLine(JSON.stringify(input))
}

function when(ts) {
  return ts ? ts.slice(0, 19).replace('T', ' ') : ''
}
</script>

<template>
  <aside
    ref="root"
    class="drawer"
    tabindex="-1"
    :role="modal ? 'dialog' : 'complementary'"
    :aria-modal="modal ? 'true' : undefined"
    :aria-label="`Transcript of ${session.title || 'untitled session'}`"
    @keydown.esc="emit('close')"
  >
    <header class="drawer-head">
      <AgentMark :agent="session.ref.agent" />
      <div class="drawer-title">
        <strong class="truncate">{{ session.title || 'Untitled session' }}</strong>
        <span class="mono faint truncate">{{ session.ref.native_id }}</span>
      </div>
      <label class="field">
        <Search :size="15" />
        <input v-model="search" placeholder="Search this transcript…" />
      </label>
      <IconButton label="Close" :icon="X" @click="$emit('close')" />
    </header>

    <div v-if="error" class="empty" style="color: var(--red)">{{ error }}</div>
    <div v-else-if="!ir" class="empty">Loading transcript…</div>

    <div v-else class="drawer-body">
      <button v-if="hidden" class="btn" style="width: 100%" @click="windowSize += 300">
        Show {{ hidden }} earlier message{{ hidden === 1 ? '' : 's' }}
      </button>

      <div v-if="!matching.length" class="empty">No messages match that text.</div>

      <div v-for="(m, mi) in visible" :key="m.source_id || mi" class="message" :class="m.role">
        <div class="message-role">
          {{ m.role }}
          <span class="faint" style="text-transform: none; font-weight: 400">{{
            when(m.timestamp)
          }}</span>
        </div>

        <template v-for="(p, pi) in m.parts" :key="pi">
          <div v-if="p.type === 'text'" class="message-text">{{ p.text }}</div>

          <div v-else-if="p.type === 'reasoning'" class="part-reasoning">
            <Brain :size="14" style="flex-shrink: 0; margin-top: 3px" />
            <span>{{ reasoningText(p) }}</span>
          </div>

          <div v-else-if="p.type === 'tool_call'" class="part-tool">
            <Wrench :size="14" style="flex-shrink: 0" />
            <strong>{{ p.name }}</strong>
            <span class="arg">{{ inputSummary(p.input) }}</span>
          </div>

          <div
            v-else-if="p.type === 'tool_result'"
            class="part-result"
            :class="{ error: p.is_error }"
            role="button"
            tabindex="0"
            @click="toggle(`${mi}:${pi}`)"
            @keyup.enter="toggle(`${mi}:${pi}`)"
          >
            <component
              :is="expanded.has(`${mi}:${pi}`) ? ChevronDown : ChevronRight"
              :size="14"
              style="flex-shrink: 0; margin-top: 3px"
            />
            <pre v-if="expanded.has(`${mi}:${pi}`)">{{ p.output }}</pre>
            <span v-else>{{ p.is_error ? 'Failed' : 'Result' }}: {{ firstLine(p.output) }}</span>
          </div>

          <div v-else-if="p.type === 'file'" class="part-reasoning">
            <Paperclip :size="14" style="flex-shrink: 0; margin-top: 3px" />
            <span>{{ p.path || 'Attached file' }}</span>
          </div>

          <div v-else-if="p.type === 'agent'" class="part-reasoning">
            <CornerDownRight :size="14" style="flex-shrink: 0; margin-top: 3px" />
            <span>
              Subagent <strong>{{ p.name }}</strong> — {{ p.transcript.length }} turn{{
                p.transcript.length === 1 ? '' : 's'
              }}
            </span>
          </div>
        </template>
      </div>
    </div>
  </aside>
</template>
