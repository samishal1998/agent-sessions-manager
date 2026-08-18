<script setup>
import { computed, ref, watch } from 'vue'
import api from './api.js'

const props = defineProps({ session: { type: Object, required: true } })
defineEmits(['close'])

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
  const needle = search.value.toLowerCase()
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

function firstLine(text) {
  const line = (text || '').split('\n', 1)[0]
  return line.length > 160 ? line.slice(0, 160) + '…' : line
}

function inputSummary(input) {
  if (!input || typeof input !== 'object') return ''
  for (const key of ['command', 'file_path', 'path', 'pattern', 'query', 'url']) {
    if (typeof input[key] === 'string') return input[key]
  }
  return firstLine(JSON.stringify(input))
}
</script>

<template>
  <div class="drawer">
    <header>
      <div>
        <strong>{{ session.title || '(untitled)' }}</strong>
        <span class="dim mono"> {{ session.ref.native_id }}</span>
      </div>
      <input v-model="search" placeholder="search in transcript…" />
      <button @click="$emit('close')">close</button>
    </header>

    <div v-if="error" class="error">{{ error }}</div>
    <div v-else-if="!ir" class="dim loading">loading transcript…</div>
    <div v-else class="messages">
      <button v-if="hidden" class="earlier" @click="windowSize += 300">
        show {{ hidden }} earlier messages
      </button>
      <div
        v-for="(m, mi) in visible"
        :key="m.source_id || mi"
        class="message"
        :class="m.role"
      >
        <div class="role">
          {{ m.role }} <span class="dim">{{ m.timestamp?.slice(0, 19).replace('T', ' ') }}</span>
        </div>
        <template v-for="(p, pi) in m.parts" :key="pi">
          <pre v-if="p.type === 'text'" class="text">{{ p.text }}</pre>
          <div v-else-if="p.type === 'reasoning'" class="reasoning">
            (thinking) {{ firstLine(p.summary) }}
          </div>
          <div v-else-if="p.type === 'tool_call'" class="tool">
            ⚙ <strong>{{ p.name }}</strong> <span class="mono">{{ inputSummary(p.input) }}</span>
          </div>
          <div
            v-else-if="p.type === 'tool_result'"
            class="tool-result"
            :class="{ error: p.is_error }"
            @click="toggle(`${mi}:${pi}`)"
          >
            <template v-if="expanded.has(`${mi}:${pi}`)">
              <pre class="mono">{{ p.output }}</pre>
            </template>
            <template v-else>
              ↳ [{{ p.is_error ? 'error' : 'ok' }}] {{ firstLine(p.output) }}
              <span class="dim">(click to expand)</span>
            </template>
          </div>
          <div v-else-if="p.type === 'agent'" class="agent-part">
            ↳ subagent <strong>{{ p.name }}</strong> — {{ p.transcript.length }} turns
          </div>
        </template>
      </div>
    </div>
  </div>
</template>
