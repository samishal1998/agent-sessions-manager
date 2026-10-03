<script setup>
import { ref, watch } from 'vue'
import { HAlert, HBadge, HButton, HSheet, HSkeleton } from '@hearth-ui/vue'
import AgentMark from '../AgentMark.vue'
import IrTranscript from '../IrTranscript.vue'
import { AdminError } from '../../admin-api.js'
import { ago, bytes } from '../../format.js'
import { notify } from '../../toasts.js'

// The stored conversation of one hub session, in a wide sheet.
const props = defineProps({ session: { type: Object, default: null }, api: { type: Object, required: true }, sid: { type: String, default: '' } })
const emit = defineEmits(['close', 'remove', 'expired'])

const state = ref('loading') // loading | ok | unavailable | error
const data = ref(null)
const message = ref('')

async function load() {
  const s = props.session
  if (!s) return
  state.value = 'loading'
  data.value = null
  try {
    const r = await props.api.transcript(s.agent, s.id)
    if (props.session !== s) return
    if (r.available && r.ir) {
      data.value = r
      state.value = 'ok'
    } else {
      message.value = r.reason || 'The hub has no stored conversation for this session.'
      state.value = 'unavailable'
    }
  } catch (e) {
    if (props.session !== s) return
    if (e instanceof AdminError && e.status === 401) return emit('expired')
    message.value = e instanceof AdminError && e.status === 404 ? 'The hub no longer has this session. It may have been deleted. Close this panel and refresh the list.' : `${e.message}. Check that the hub is running, then try again.`
    state.value = 'error'
  }
}
watch(() => props.session, load, { immediate: true })

async function copyId() {
  try {
    await navigator.clipboard.writeText(props.session.id)
    notify('Session id copied.')
  } catch {
    notify('Could not copy', { tone: 'danger', description: 'The browser blocked the clipboard. Select the id and copy it by hand.' })
  }
}
// The hub keeps paths home-relative as ${HOME}/…; read them as ~/….
const project = (s) => (s.project || '—').replace('${HOME}', '~')
</script>

<template>
  <HSheet :open="!!session" :title="session?.title || 'Untitled session'" width="min(900px, 100vw)" @close="emit('close')">
    <div v-if="session" class="ts">
      <div class="ts-head">
        <span class="ts-agent"><AgentMark :agent="session.agent" :size="16" /> {{ session.agent }}</span>
        <button type="button" class="admin-link admin-mono" :title="`Copy ${session.id}`" :aria-label="`Copy session id ${session.id}`" @click="copyId">{{ sid }}</button>
        <HBadge tone="neutral" :label="`${session.revisions} ${session.revisions === 1 ? 'revision' : 'revisions'}`" />
        <HBadge tone="neutral" :label="bytes(session.size)" />
      </div>
      <dl class="ts-facts">
        <div><dt>Project</dt><dd class="ts-wrap">{{ project(session) }}</dd></div>
        <div><dt>Pushed by</dt><dd>{{ session.machine || '—' }}</dd></div>
        <div><dt>Pushed</dt><dd>{{ ago(session.pushed_at) }}</dd></div>
      </dl>

      <HSkeleton v-if="state === 'loading'" :lines="6" label="Loading the conversation" />
      <HAlert v-else-if="state === 'unavailable'" tone="info" title="No conversation to show" :description="message" />
      <HAlert v-else-if="state === 'error'" tone="danger" title="Could not load the conversation" :description="message">
      </HAlert>
      <HButton v-if="state === 'error'" size="compact" label="Try again" @click="load" />
      <IrTranscript v-else-if="state === 'ok'" :ir="data.ir" :truncated="data.truncated" />
    </div>
    <template #footer>
      <HButton variant="ghost" label="Close" @click="emit('close')" />
      <HButton v-if="session" variant="danger" label="Delete from the hub" @click="emit('remove', session)" />
    </template>
  </HSheet>
</template>

<style>
.ts { display: grid; grid-template-columns: minmax(0, 1fr); gap: 16px; min-width: 0; }
.ts-head { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
.ts-agent { display: inline-flex; align-items: center; gap: 6px; font-weight: 600; }
.ts-facts { display: flex; flex-wrap: wrap; gap: 8px 24px; margin: 0; }
.ts-facts div { min-width: 0; max-width: 100%; }
.ts-facts dt { font-size: 12px; color: var(--h-muted); }
.ts-facts dd { margin: 0; }
.ts-wrap { overflow-wrap: anywhere; }
</style>
