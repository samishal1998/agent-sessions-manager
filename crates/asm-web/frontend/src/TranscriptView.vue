<script setup>
import { HAccordion, HAlert, HBadge, HButton, HCodeBlock, HEmptyState, HInput, HSkeleton, HTextarea } from '@hearth-ui/vue'
import './styles/transcript.css'
import { computed, onBeforeUnmount, onMounted, nextTick, ref, watch } from 'vue'
import {
  Brain,
  Check,
  Copy,
  CornerDownRight,
  Paperclip,
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

// A session another terminal is driving right now is refused by the core —
// none of these CLIs promise to handle two writers — so the composer says
// so up front instead of letting the user type a message and lose it.
const isLive = computed(() => props.session.status?.state === 'live')
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

const copied = ref(false)
let copiedTimer
async function copyId() {
  try {
    await navigator.clipboard.writeText(props.session.ref.native_id)
    copied.value = true
    clearTimeout(copiedTimer)
    copiedTimer = setTimeout(() => (copied.value = false), 1500)
  } catch {
    /* clipboard blocked (insecure origin); the id is selectable text anyway */
  }
}
onBeforeUnmount(() => clearTimeout(copiedTimer))

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
// Each tool result is its own one-item accordion; its id doubles as the key.
function resultItems(key, p) {
  return [{ id: key, title: `${p.is_error ? 'Failed' : 'Result'}: ${firstLine(p.output) || '(empty)'}` }]
}
const roleTone = { user: 'accent', assistant: 'neutral' }

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
    class="drawer tv"
    tabindex="-1"
    :role="modal ? 'dialog' : 'complementary'"
    :aria-modal="modal ? 'true' : undefined"
    :aria-label="`Transcript: ${session.title || 'untitled session'}`"
    @keydown.esc="emit('close')"
  >
    <header class="tv-head">
      <div class="tv-head-row">
        <AgentMark :agent="session.ref.agent" />
        <strong class="tv-title" :title="session.title || 'Untitled session'">{{ session.title || 'Untitled session' }}</strong>
        <IconButton label="Close transcript" :icon="X" @click="$emit('close')" />
      </div>
      <div class="tv-idline">
        <button
          type="button"
          class="tv-copy"
          :aria-label="`Copy session ID ${session.ref.native_id}`"
          :title="session.ref.native_id"
          @click="copyId"
        >
          <component :is="copied ? Check : Copy" :size="14" aria-hidden="true" />
          <span class="tv-id">{{ session.ref.native_id }}</span>
        </button>
        <span v-if="session.size_bytes != null">{{ size }}</span>
        <span class="tv-visually-hidden" role="status">{{ copied ? 'Session ID copied' : '' }}</span>
      </div>
      <div class="tv-search"><HInput v-model="search" type="search" label="Search this transcript" placeholder="Search this transcript…" /></div>
    </header>

    <HAlert v-if="error" tone="danger" title="Could not load the transcript" :description="error" />
    <HSkeleton v-else-if="!ir" :lines="6" label="Loading transcript" />

    <div v-else ref="body" class="tv-body">
      <HButton v-if="hidden" class="tv-more" variant="secondary" size="compact" @click="windowSize += 300">
        Show {{ hidden }} earlier message{{ hidden === 1 ? '' : 's' }}
      </HButton>

      <HEmptyState v-if="!matching.length" icon="search" title="No messages match" description="Try different text." />

      <section v-for="(m, mi) in visible" :key="m.source_id || mi" class="tv-message" :aria-label="`${m.role} message`">
        <div class="tv-meta">
          <HBadge :tone="roleTone[m.role] ?? 'info'" :label="m.role" />
          <span>{{ when(m.timestamp) }}</span>
        </div>

        <template v-for="(p, pi) in m.parts" :key="pi">
          <template v-if="p.type === 'text'">
            <MarkupBlock v-if="isStructured(p.text)" :nodes="markup(p.text)" />
            <div v-else class="tv-text">{{ p.text }}</div>
          </template>

          <div v-else-if="p.type === 'reasoning'" class="tv-part">
            <Brain :size="14" aria-hidden="true" />
            <span>{{ reasoningText(p) }}</span>
          </div>

          <div v-else-if="p.type === 'tool_call'" class="tv-part">
            <Wrench :size="14" aria-hidden="true" />
            <strong>{{ p.name }}</strong>
            <span class="tv-arg">{{ inputSummary(p.input) }}</span>
          </div>

          <HAccordion
            v-else-if="p.type === 'tool_result'"
            :items="resultItems(`${mi}:${pi}`, p)"
            :model-value="expanded.has(`${mi}:${pi}`) ? [`${mi}:${pi}`] : []"
            @update:model-value="toggle(`${mi}:${pi}`)"
          >
            <template #[`${mi}:${pi}`]>
              <template v-if="expanded.has(`${mi}:${pi}`)">
                <MarkupBlock v-if="isStructured(p.output)" :nodes="markup(p.output)" />
                <HCodeBlock v-else :code="p.output || ''" wrap />
              </template>
            </template>
          </HAccordion>

          <div v-else-if="p.type === 'file'" class="tv-part">
            <Paperclip :size="14" aria-hidden="true" />
            <span>{{ p.path || 'Attached file' }}</span>
          </div>

          <div v-else-if="p.type === 'agent'" class="tv-part">
            <CornerDownRight :size="14" aria-hidden="true" />
            <span>
              Subagent <strong>{{ p.name }}</strong> — {{ p.transcript.length }} turn{{
                p.transcript.length === 1 ? '' : 's'
              }}
            </span>
          </div>
        </template>
      </section>

      <section v-if="streaming.length" class="tv-message tv-live" aria-label="Turn in flight">
        <div class="tv-meta">
          <HBadge tone="warning" dot label="in flight" />
          <span>not yet written to the session</span>
        </div>
        <template v-for="(e, ei) in streaming" :key="ei">
          <div v-if="e.event === 'text'" class="tv-text">{{ e.text }}</div>
          <div v-else-if="e.event === 'reasoning'" class="tv-part">
            <Brain :size="14" aria-hidden="true" />
            <span>{{ firstLine(e.text) }}</span>
          </div>
          <div v-else-if="e.event === 'tool_call'" class="tv-part">
            <Wrench :size="14" aria-hidden="true" />
            <strong>{{ e.name }}</strong>
            <span class="tv-arg">{{ e.detail }}</span>
          </div>
          <div v-else-if="e.event === 'tool_result'" class="tv-part">
            <span>{{ e.is_error ? 'Failed' : 'Result' }}: {{ firstLine(e.output) }}</span>
          </div>
          <!-- A line asm did not recognise. Shown, not dropped: these
               formats change, and a swallowed line reads as silence. -->
          <div v-else-if="e.event === 'raw'" class="tv-part">
            <span class="tv-arg">{{ firstLine(e.line) }}</span>
          </div>
        </template>
      </section>
    </div>

    <form v-if="ir" class="tv-composer" @submit.prevent="sendDraft">
      <HAlert v-if="sendError" tone="danger" :description="sendError" />
      <HAlert v-if="!canSend" tone="info" :description="`asm cannot send into ${session.ref.agent} sessions yet.`" />
      <HAlert
        v-else-if="isLive"
        tone="warning"
        :description="`This session is being driven by another ${session.ref.agent} process right now. Close it to reply from here.`"
      />
      <template v-else>
        <HTextarea
          v-model="draft"
          label="Reply"
          :rows="2"
          :disabled="busy"
          :placeholder="`Reply in this ${session.ref.agent} session…`"
          @keydown.enter.exact.prevent="sendDraft"
        />
        <div class="tv-actions">
          <span>
            {{ busy ? 'The agent is working. It can read and edit files in this project.'
                    : 'Enter sends · Shift+Enter for a new line' }}
          </span>
          <HButton v-if="busy" type="button" variant="danger" size="compact" @click="stopSending">
            <Square :size="14" aria-hidden="true" /> Stop
          </HButton>
          <HButton v-else type="submit" variant="secondary" size="compact" :disabled="!draft.trim()">
            <SendHorizontal :size="14" aria-hidden="true" /> Send
          </HButton>
        </div>
      </template>
    </form>
  </aside>
</template>
