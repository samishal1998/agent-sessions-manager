import { computed, ref, watchEffect } from 'vue'
import { TOKENS } from './theme.js'

// The colour mode the person chose (light, dark or follow the system) and the
// mode it resolves to now. One shared state per page, remembered per browser.
const KEY = 'asm-theme'
const read = () => {
  try {
    const v = localStorage.getItem(KEY)
    return v === 'light' || v === 'dark' ? v : 'system'
  } catch {
    return 'system'
  }
}
const preference = ref(read())
const query = typeof matchMedia === 'function' ? matchMedia('(prefers-color-scheme: light)') : null
const systemLight = ref(!!query?.matches)
query?.addEventListener?.('change', (e) => (systemLight.value = e.matches))

const resolved = computed(() =>
  preference.value === 'system' ? (systemLight.value ? 'light' : 'dark') : preference.value,
)
const tokens = computed(() => TOKENS[resolved.value])

export function useTheme() {
  // The page behind the app (overscroll, first paint) follows the mode too.
  watchEffect(() => {
    document.documentElement.dataset.asmMode = resolved.value
    document.documentElement.style.colorScheme = resolved.value
  })
  const setPreference = (v) => {
    preference.value = v
    try {
      localStorage.setItem(KEY, v)
    } catch {
      /* private window: the choice lasts until the page closes */
    }
  }
  return { preference, resolved, tokens, setPreference }
}
