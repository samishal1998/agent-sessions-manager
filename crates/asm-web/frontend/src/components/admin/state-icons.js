import { Ban, CircleDashed, Hourglass, LoaderCircle, SkipForward } from 'lucide-vue-next'

// One icon per state, so a badge never leans on colour alone: Hearth's where it
// has one, lucide for the rest.
export const STATE_ICON = { pending: CircleDashed, queued: 'clock', running: LoaderCircle, ok: 'check', blocked: 'alert', cancelled: Ban, expired: Hourglass, skipped: SkipForward }
