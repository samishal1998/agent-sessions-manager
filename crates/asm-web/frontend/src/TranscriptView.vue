<script setup>
import { computed, onBeforeUnmount, onMounted, nextTick, ref, watch } from 'vue'
import {
  Brain,
  ChevronDown,
  ChevronRight,
  CornerDownRight,
  Paperclip,
  Search,
  SendHorizontal,
  Square,
  Wrench,
  X,
} from 'lucide-vue-next'
import api from './api.js'
import { hasMarkup, parseMarkup } from './markup.js'
import AgentMark from './components/AgentMark.vue'
import IconButton from './components/IconButton.vue'
import MarkupBlock from './components/MarkupBlock.vue'

const props = defineProps({
  session: { type: Object, required: true },
  // When the drawer covers the page rather than sitting beside it, it is a
  // modal dialog and has to say so.
  modal: { type: Boolean, default: false },
  // Whether this agent can be sent a new message at all. Codex, Claude Code
  // and OpenCode can; jcode cannot, and the composer says so instead of
  // offering a box that always fails.
  canSend: { type: Boolean, default: false },
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

const size = computed(() => {
  const bytes = props.session.size_bytes
  if (bytes == null) return ''
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return unit === 0
    ? `${bytes} B`
    : `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`
})

function when(ts) {
  return ts ? ts.slice(0, 19).replace('T', ' ') : ''
}

// Agents embed XML-ish envelopes in message text. Parsing is memoised
// because a long transcript re-renders on every scroll and filter change.
const markupCache = new Map()
function markup(text) {
  if (!markupCache.has(text)) markupCache.set(text, parseMarkup(text))
  return markupCache.get(text)
}
function isStructured(text) {
  return hasMarkup(markup(text))
}
watch(
  () => props.session,
  () => markupCache.clear(),
)

/* Composing a reply ---------------------------------------------------- */

const draft = ref('')
// Events from the turn in flight. They are rendered under the transcript
// rather than merged into it: the transcript is what the agent has written
// to disk, and these have not landed there yet.
const streaming = ref([])
const sendError = ref('')
const busy = ref(false)
let inFlight = null
const body = ref(null)

// The turn only reaches the store when the agent finishes writing it, so
// the transcript is reloaded at the end rather than patched as it streams.
async function reloadTranscript() {
  try {
    ir.value = await api.ir(props.session)
  } catch (e) {
    error.value = e.message
  }
}

function scrollToBottom() {
  nextTick(() => {
    const el = body.value
    if (el) el.scrollTop = el.scrollHeight
  })
}

async function sendDraft() {
  const message = draft.value.trim()
  if (!message || busy.value) return
  busy.value = true
  sendError.value = ''
  streaming.value = [{ event: 'text', role: 'user', text: message }]
  draft.value = ''
  scrollToBottom()

  inFlight = api.send(props.session, message, (event) => {
    // `started` only confirms which session the agent thinks it is
    // continuing; a mismatch would mean it forked, which is worth saying
    // out loud rather than silently rendering into the wrong transcript.
    if (event.event === 'started') {
      if (event.session_id && event.session_id !== props.session.ref.native_id) {
        sendError.value = `the agent replied in ${event.session_id}, not this session`
      }
      return
    }
    if (event.event === 'usage') return
    if (event.event === 'done') {
      if (!event.ok) sendError.value = event.error || 'the turn failed'
      return
    }
    streaming.value = [...streaming.value, event]
    scrollToBottom()
  })

  try {
    await inFlight.done
  } catch (e) {
    if (e.name !== 'AbortError') sendError.value = e.message
  } finally {
    inFlight = null
    busy.value = false
    await reloadTranscript()
    // Everything that landed is now in the transcript proper; keeping the
    // streamed copy would show every reply twice.
    if (!sendError.value) streaming.value = []
    scrollToBottom()
  }
}

function stopSending() {
  inFlight?.abort()
}

// Leaving mid-turn must not leave an agent running against a page nobody is
// watching; the server kills it when the stream drops.
onBeforeUnmount(() => inFlight?.abort())
watch(
  () => props.session,
  () => {
    inFlight?.abort()
    streaming.value = []
    sendError.value = ''
    draft.value = ''
    busy.value = false
  },
)
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
        <span class="mono faint truncate">
          {{ session.ref.native_id
          }}<template v-if="session.size_bytes != null"> · {{ size }}</template>
        </span>
      </div>
      <label class="field">
        <Search :size="15" />
        <input v-model="search" placeholder="Search this transcript…" />
      </label>
      <IconButton label="Close" :icon="X" @click="$emit('close')" />
    </header>

    <div v-if="error" class="empty" style="color: var(--red)">{{ error }}</div>
    <div v-else-if="!ir" class="empty">Loading transcript…</div>

    <div v-else ref="body" class="drawer-body">
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
          <template v-if="p.type === 'text'">
            <MarkupBlock v-if="isStructured(p.text)" :nodes="markup(p.text)" />
            <div v-else class="message-text">{{ p.text }}</div>
          </template>

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
            <template v-if="expanded.has(`${mi}:${pi}`)">
              <MarkupBlock
                v-if="isStructured(p.output)"
                :nodes="markup(p.output)"
                style="width: 100%"
              />
              <pre v-else>{{ p.output }}</pre>
            </template>
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

      <div v-if="streaming.length" class="message live">
        <div class="message-role">
          in flight
          <span class="faint" style="text-transform: none; font-weight: 400">
            not yet written to the session
          </span>
        </div>
        <template v-for="(e, ei) in streaming" :key="ei">
          <div v-if="e.event === 'text'" class="message-text">{{ e.text }}</div>
          <div v-else-if="e.event === 'reasoning'" class="part-reasoning">
            <Brain :size="14" style="flex-shrink: 0; margin-top: 3px" />
            <span>{{ firstLine(e.text) }}</span>
          </div>
          <div v-else-if="e.event === 'tool_call'" class="part-tool">
            <Wrench :size="14" style="flex-shrink: 0" />
            <strong>{{ e.name }}</strong>
            <span class="arg">{{ e.detail }}</span>
          </div>
          <div
            v-else-if="e.event === 'tool_result'"
            class="part-result"
            :class="{ error: e.is_error }"
          >
            <span>{{ e.is_error ? 'Failed' : 'Result' }}: {{ firstLine(e.output) }}</span>
          </div>
          <!-- A line asm did not recognise. Shown, not dropped: these
               formats change, and a swallowed line reads as silence. -->
          <div v-else-if="e.event === 'raw'" class="part-reasoning">
            <span class="mono">{{ firstLine(e.line) }}</span>
          </div>
        </template>
      </div>
    </div>

    <form v-if="ir" class="composer" @submit.prevent="sendDraft">
      <p v-if="sendError" class="composer-error">{{ sendError }}</p>
      <p v-if="!canSend" class="composer-off">
        asm cannot send into {{ session.ref.agent }} sessions yet.
      </p>
      <template v-else>
        <textarea
          v-model="draft"
          class="composer-input"
          rows="2"
          :disabled="busy"
          :placeholder="`Reply in this ${session.ref.agent} session…`"
          @keydown.enter.exact.prevent="sendDraft"
        ></textarea>
        <div class="composer-actions">
          <span class="faint">
            {{ busy ? 'The agent is working — it can read and edit files in this project.'
                    : 'Enter sends · Shift+Enter for a new line' }}
          </span>
          <button v-if="busy" type="button" class="btn danger" @click="stopSending">
            <Square :size="14" /> Stop
          </button>
          <button v-else type="submit" class="btn" :disabled="!draft.trim()">
            <SendHorizontal :size="14" /> Send
          </button>
        </div>
      </template>
    </form>
  </aside>
</template>
