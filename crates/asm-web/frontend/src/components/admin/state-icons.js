import { Ban, Check, CircleDashed, Clock, Hourglass, LoaderCircle, SkipForward, TriangleAlert } from 'lucide-vue-next'

// One icon per state, so a badge never leans on colour alone.
export const STATE_ICON = { pending: CircleDashed, queued: Clock, running: LoaderCircle, ok: Check, blocked: TriangleAlert, cancelled: Ban, expired: Hourglass, skipped: SkipForward }
