import { Bot, Orbit, Sparkles, SquareTerminal, Terminal, Zap } from 'lucide-vue-next'

// The one place an agent's name and icon live.
//
// There used to be two: AgentMark had this table and the agent filter had
// its own copy, so adding Codex and Antigravity gave them row icons while
// the filter quietly went on offering three agents. A filter that cannot
// name an agent it is showing you is worse than no filter, and nothing in
// the build could notice. Same story for the resume commands, which lived
// in a third table and so handed out a bare session id for the two new
// agents.
//
// `resume` must agree with each adapter's resume_command in asm-core.
export const AGENTS = {
  'claude-code': {
    label: 'Claude Code',
    icon: Sparkles,
    resume: (s) => `claude --resume ${s.ref.native_id}`,
  },
  opencode: {
    label: 'OpenCode',
    icon: SquareTerminal,
    resume: (s) => `opencode -s ${s.ref.native_id}`,
  },
  jcode: {
    label: 'jcode',
    icon: Zap,
    // jcode resolves --resume by memorable short name or by id.
    resume: (s) => `jcode --resume ${s.slug || s.ref.native_id}`,
  },
  codex: {
    label: 'Codex',
    icon: Terminal,
    resume: (s) => `codex resume ${s.ref.native_id}`,
  },
  antigravity: {
    label: 'Antigravity',
    icon: Orbit,
    resume: (s) => `agy --conversation ${s.ref.native_id}`,
  },
}

/// Display order, so the filter and any other list agree.
export const AGENT_ORDER = Object.keys(AGENTS)

/// An agent asm has never heard of still gets a name and an icon: the
/// server is the authority on what exists, not this file.
export function agentMeta(id) {
  return AGENTS[id] ?? { label: id, icon: Bot }
}

/// The command that reopens a session in its own agent. Falls back to the
/// bare id, which is at least the part you would need to look it up.
export function resumeCommand(session) {
  return AGENTS[session.ref.agent]?.resume?.(session) ?? session.ref.native_id
}

/// Filter options for the agents actually present, known ones first in
/// `AGENT_ORDER` and anything unrecognised after, alphabetically.
export function agentOptions(present) {
  const ids = [...new Set(present)].filter(Boolean)
  const known = AGENT_ORDER.filter((id) => ids.includes(id))
  const unknown = ids.filter((id) => !(id in AGENTS)).sort()
  return [...known, ...unknown].map((value) => ({ value, label: agentMeta(value).label, icon: agentMeta(value).icon }))
}
