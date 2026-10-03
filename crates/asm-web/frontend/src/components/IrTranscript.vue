<script setup>
import { computed, ref, watch } from 'vue'
import { HAccordion, HAlert, HBadge, HButton, HCodeBlock, HEmptyState, HInput } from '@hearth-ui/vue'

// A stored conversation (Session IR v1), role by role. Only the newest
// messages are drawn at first: a long session is thousands of nodes otherwise.
const props = defineProps({
  ir: { type: Object, required: true },
  truncated: { type: Boolean, default: false },
  cap: { type: Number, default: 2000 },
})

const search = ref('')
const windowSize = ref(100)
const open = ref(new Set())
watch(() => props.ir, () => { search.value = ''; windowSize.value = 100; open.value = new Set() })

const partText = (p) => [p.text, p.summary, p.output, p.name, p.input && JSON.stringify(p.input), p.path, p.description].filter(Boolean).join('\n')
// Keep each message's index in the full list: it keys the open accordions.
const matching = computed(() => {
  const q = search.value.trim().toLowerCase()
  const all = (props.ir.messages || []).map((m, i) => ({ m, i }))
  return q ? all.filter(({ m }) => m.role.includes(q) || m.parts.some((p) => partText(p).toLowerCase().includes(q))) : all
})
const visible = computed(() => matching.value.slice(-windowSize.value))
const hidden = computed(() => matching.value.length - visible.value.length)

const tone = { user: 'accent', assistant: 'neutral', system: 'warning' }
const when = (ts) => (ts ? ts.slice(0, 16).replace('T', ' ') : '')
const first = (t) => {
  const l = (t || '').split('\n').find((x) => x.trim()) ?? ''
  return l.length > 120 ? `${l.slice(0, 120)}…` : l
}
const hint = (i) => {
  const v = i && typeof i === 'object' ? Object.values(i).find((x) => typeof x === 'string' && x) : i
  return typeof v === 'string' ? first(v).slice(0, 80) : ''
}
const json = (v) => (typeof v === 'string' ? v : JSON.stringify(v, null, 2))
const toggle = (k) => {
  const s = new Set(open.value)
  s.has(k) ? s.delete(k) : s.add(k)
  open.value = s
}
const item = (k, title) => [{ id: k, title }]
</script>

<template>
  <div class="irt">
    <HInput v-model="search" type="search" label="Search this conversation" placeholder="Search messages, tools, results…" />
    <HAlert v-if="truncated" tone="info" :description="`Showing the last ${cap} messages. Earlier ones are on the machine that pushed this session.`" />
    <p class="irt-count" role="status">{{ search ? `${matching.length} of ${ir.messages.length} messages match` : `${ir.messages.length} message${ir.messages.length === 1 ? '' : 's'}` }}</p>

    <HEmptyState v-if="!ir.messages.length" icon="search" title="This session has no messages" description="It was pushed without a conversation." />
    <HEmptyState v-else-if="!matching.length" icon="search" title="No message matches" description="Try a different word.">
      <HButton size="compact" label="Clear search" @click="search = ''" />
    </HEmptyState>

    <HButton v-if="hidden > 0" class="irt-more" variant="secondary" size="compact" :label="`Show ${Math.min(hidden, 200)} earlier message${hidden === 1 ? '' : 's'} (${hidden} hidden)`" @click="windowSize += 200" />

    <ol class="irt-list" aria-label="Messages, oldest first">
      <li v-for="{ m, i } in visible" :key="i" class="irt-msg" :class="`role-${m.role}`">
        <div class="irt-meta">
          <HBadge :tone="tone[m.role] ?? 'info'" :label="m.role" />
          <time v-if="m.timestamp" :datetime="m.timestamp">{{ when(m.timestamp) }}</time>
        </div>
        <template v-for="(p, pi) in m.parts" :key="pi">
          <div v-if="p.type === 'text'" class="irt-text">{{ p.text }}</div>

          <HAccordion v-else-if="p.type === 'reasoning'" :items="item(`${i}:${pi}`, `Reasoning: ${first(p.summary) || 'not recorded in the transcript'}`)"
            :model-value="open.has(`${i}:${pi}`) ? [`${i}:${pi}`] : []" @update:model-value="toggle(`${i}:${pi}`)">
            <template #[`${i}:${pi}`]><div v-if="open.has(`${i}:${pi}`)" class="irt-text">{{ p.summary || 'The agent reasoned here; the provider kept the text private.' }}</div></template>
          </HAccordion>

          <HAccordion v-else-if="p.type === 'tool_call'" :items="item(`${i}:${pi}`, `Tool: ${p.name}${hint(p.input) ? ` · ${hint(p.input)}` : ''}`)"
            :model-value="open.has(`${i}:${pi}`) ? [`${i}:${pi}`] : []" @update:model-value="toggle(`${i}:${pi}`)">
            <template #[`${i}:${pi}`]><HCodeBlock v-if="open.has(`${i}:${pi}`)" :code="json(p.input)" language="json" title="Input" wrap /></template>
          </HAccordion>

          <HAccordion v-else-if="p.type === 'tool_result'" :items="item(`${i}:${pi}`, `${p.is_error ? 'Failed' : 'Result'}: ${first(p.output) || '(empty)'}`)"
            :model-value="open.has(`${i}:${pi}`) ? [`${i}:${pi}`] : []" @update:model-value="toggle(`${i}:${pi}`)">
            <template #[`${i}:${pi}`]>
              <HCodeBlock v-if="open.has(`${i}:${pi}`)" :code="p.output || ''" title="Output" wrap />
              <p v-if="open.has(`${i}:${pi}`) && p.truncated" class="irt-count">The agent cut this output short.</p>
            </template>
          </HAccordion>

          <p v-else-if="p.type === 'file'" class="irt-file">File: <span class="admin-mono">{{ p.path || p.mime || 'attachment' }}</span></p>
          <p v-else-if="p.type === 'agent'" class="irt-file">Subagent {{ p.name }}<template v-if="p.description"> · {{ p.description }}</template> · {{ p.transcript?.length || 0 }} messages</p>
        </template>
      </li>
    </ol>
  </div>
</template>

<style>
.irt { display: grid; grid-template-columns: minmax(0, 1fr); gap: 12px; min-width: 0; }
.irt-count { margin: 0; font-size: 13px; color: var(--h-muted); }
.irt-more { justify-self: start; }
.irt-list { grid-template-columns: minmax(0, 1fr); list-style: none; margin: 0; padding: 0; display: grid; gap: 12px; }
.irt-msg { display: grid; gap: 8px; min-width: 0; padding: 12px 16px; border: 1px solid var(--h-border); border-radius: 8px; background: var(--h-surface); }
.irt-msg.role-user { border-inline-start: 3px solid var(--h-accent); }
.irt-meta { display: flex; align-items: center; gap: 8px; font-size: 13px; color: var(--h-muted); }
.irt-text { white-space: pre-wrap; overflow-wrap: anywhere; line-height: 1.55; }
.irt-file { margin: 0; font-size: 13px; color: var(--h-muted); overflow-wrap: anywhere; }
</style>
