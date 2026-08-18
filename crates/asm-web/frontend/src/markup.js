// Agents wrap things in XML-ish envelopes inside otherwise plain message
// text: injected context, slash-command echoes, background-task
// notifications, subagent results, tool errors. Rendered as plain text they
// are a wall of angle brackets; parsed, they collapse into labelled blocks.
//
// The hard part is NOT parsing — it is deciding what is markup at all.
// Transcripts are full of tool output containing source code, and code is
// full of things that look like tags: `Vec<T>`, `Option<String>`,
// `HashMap<K, V>`, `<div>`, `<any>`. A scan of the transcripts on this
// machine found 400 distinct "tags", almost all of them generics. So the
// rule here is deliberately narrow:
//
//   1. the name is either explicitly known, or contains a - or _
//      (which excludes <T>, <String>, <div>, <void>, <Diagnostic>);
//   2. the name is lowercase (code generics are typically capitalised);
//   3. a matching closing tag exists, with nesting of the same name
//      balanced;
//   4. the opening tag starts a line.
//
// Anything failing those stays plain text. Nothing is ever dropped: text
// outside recognised elements is preserved verbatim.

/** Envelopes we understand well enough to label. */
export const KNOWN = {
  'system-reminder': { label: 'System reminder', tone: 'note', collapsed: true },
  'task-notification': { label: 'Background task', tone: 'note', collapsed: true },
  task_result: { label: 'Subagent result', tone: 'note', collapsed: true },
  tool_use_error: { label: 'Tool error', tone: 'error', collapsed: false },
  'persisted-output': { label: 'Output saved to a file', tone: 'muted', collapsed: true },
  'local-command-caveat': { label: 'Note', tone: 'muted', collapsed: true },
  'command-name': { label: 'Command', tone: 'note', collapsed: false },
  'command-message': { label: 'Command message', tone: 'muted', collapsed: false },
  'command-args': { label: 'Command arguments', tone: 'muted', collapsed: false },
  'local-command-stdout': { label: 'Command output', tone: 'muted', collapsed: false },
  'bash-input': { label: 'Shell input', tone: 'note', collapsed: false },
  'bash-stdout': { label: 'Shell output', tone: 'muted', collapsed: false },
  // Children of task-notification, meaningful on their own.
  'task-id': { label: 'Task', tone: 'muted', collapsed: false },
  'tool-use-id': { label: 'Tool use', tone: 'muted', collapsed: false },
  'output-file': { label: 'Output file', tone: 'muted', collapsed: false },
}

const NAME = '[A-Za-z][\\w.:-]*'
const OPEN = new RegExp(`<(${NAME})((?:\\s[^<>]*?)?)(/?)>`, 'g')

/**
 * Conservative at the top level: a name must be known, or lowercase with a
 * separator.
 *
 * Inside an element the rule relaxes to "lowercase", because the block is
 * already established as markup and its children are frequently single
 * words — `<task-notification>` carries `<status>`, `<summary>`, `<result>`.
 * The lowercase requirement still rejects code generics, and the caller
 * still requires a line-start position and a matching close tag.
 */
export function looksLikeEnvelope(name, inside = false) {
  if (Object.hasOwn(KNOWN, name)) return true
  if (name !== name.toLowerCase()) return false
  if (inside) return /^[a-z][a-z0-9]*(?:[-_.:][a-z0-9]+)*$/.test(name)
  return /^[a-z][a-z0-9]*(?:[-_][a-z0-9]+)+$/.test(name)
}

function parseAttrs(raw) {
  const attrs = {}
  if (!raw) return attrs
  for (const m of raw.matchAll(/([\w.:-]+)(?:\s*=\s*"([^"]*)"|\s*=\s*'([^']*)')?/g)) {
    if (m[1]) attrs[m[1]] = m[2] ?? m[3] ?? ''
  }
  return attrs
}

/** Index just past the close tag matching the open tag that ends at `from`. */
function findClose(text, name, from) {
  const open = new RegExp(`<${escape(name)}(?:\\s[^<>]*?)?>`, 'g')
  const close = new RegExp(`</${escape(name)}\\s*>`, 'g')
  let depth = 1
  let cursor = from
  while (cursor < text.length) {
    close.lastIndex = cursor
    const c = close.exec(text)
    if (!c) return null
    open.lastIndex = cursor
    let o = open.exec(text)
    while (o && o.index < c.index) {
      depth++
      open.lastIndex = o.index + o[0].length
      o = open.exec(text)
    }
    depth--
    if (depth === 0) return { innerEnd: c.index, end: c.index + c[0].length }
    cursor = c.index + c[0].length
  }
  return null
}

function escape(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

function startsLine(text, index) {
  for (let i = index - 1; i >= 0; i--) {
    const ch = text[i]
    if (ch === '\n') return true
    if (ch !== ' ' && ch !== '\t') return false
  }
  return true
}

function pushText(nodes, value) {
  if (!value) return
  const last = nodes[nodes.length - 1]
  if (last && last.type === 'text') last.value += value
  else nodes.push({ type: 'text', value })
}

/**
 * Parse a message body into text and element nodes.
 * @returns {Array<{type:'text',value:string}|{type:'element',name:string,attrs:object,children:Array}>}
 */
export function parseMarkup(text, depth = 0) {
  const nodes = []
  if (typeof text !== 'string' || !text.includes('<')) {
    pushText(nodes, text ?? '')
    return nodes
  }
  // Deeply nested documents are rendered, not recursed into forever.
  if (depth > 6) {
    pushText(nodes, text)
    return nodes
  }

  let cursor = 0
  OPEN.lastIndex = 0
  let m
  while ((m = OPEN.exec(text))) {
    const [raw, name, attrs, selfClosing] = m
    if (m.index < cursor) continue
    if (selfClosing || !looksLikeEnvelope(name, depth > 0) || !startsLine(text, m.index)) continue

    const bounds = findClose(text, name, m.index + raw.length)
    if (!bounds) continue

    pushText(nodes, text.slice(cursor, m.index))
    nodes.push({
      type: 'element',
      name,
      attrs: parseAttrs(attrs),
      children: parseMarkup(text.slice(m.index + raw.length, bounds.innerEnd), depth + 1),
    })
    cursor = bounds.end
    OPEN.lastIndex = cursor
  }
  pushText(nodes, text.slice(cursor))
  return nodes
}

/** True when parsing found structure worth rendering as such. */
export function hasMarkup(nodes) {
  return nodes.some((n) => n.type === 'element')
}

/** The text of an element, when it has no element children. */
export function textOf(node) {
  return node.children
    .map((c) => (c.type === 'text' ? c.value : textOf(c)))
    .join('')
    .trim()
}
