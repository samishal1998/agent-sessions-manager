import { expect, test } from 'bun:test'
import { AGENTS, agentMeta, agentOptions, resumeCommand } from './agents.js'

const session = (agent, native_id = 'ID', slug = null) => ({ ref: { agent, native_id }, slug })

/// A half-added agent is the bug this file exists to prevent: icons in the
/// list, but missing from the filter or handing out a bare id on copy.
test('every known agent is completely described', () => {
  for (const [id, meta] of Object.entries(AGENTS)) {
    expect(meta.label, `${id} label`).toBeTruthy()
    expect(meta.icon, `${id} icon`).toBeTruthy()
    expect(typeof meta.resume, `${id} resume`).toBe('function')
    expect(resumeCommand(session(id)), `${id} resume output`).toContain('ID')
  }
})

test('the filter offers the agents present, in a stable order', () => {
  const opts = agentOptions(['antigravity', 'claude-code', 'codex'])
  expect(opts.map((o) => o.value)).toEqual(['claude-code', 'codex', 'antigravity'])
  expect(opts.map((o) => o.label)).toEqual(['Claude Code', 'Codex', 'Antigravity'])
})

test('an agent absent from the data is not offered', () => {
  expect(agentOptions(['jcode']).map((o) => o.value)).toEqual(['jcode'])
})

/// The server is the authority on what exists. An agent added to asm-core
/// before this table must still be filterable and nameable.
test('an unknown agent is still named and offered', () => {
  const opts = agentOptions(['claude-code', 'future-agent'])
  expect(opts.map((o) => o.value)).toEqual(['claude-code', 'future-agent'])
  expect(agentMeta('future-agent').label).toBe('future-agent')
  expect(agentMeta('future-agent').icon).toBeTruthy()
  // No command is known, so the id is the most useful thing to hand over.
  expect(resumeCommand(session('future-agent', 'abc'))).toBe('abc')
})

test('duplicates and blanks collapse', () => {
  expect(agentOptions(['codex', 'codex', '', null]).map((o) => o.value)).toEqual(['codex'])
})
