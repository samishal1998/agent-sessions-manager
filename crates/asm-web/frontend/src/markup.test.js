import { describe, expect, test } from 'bun:test'
import { hasMarkup, looksLikeEnvelope, parseMarkup, textOf } from './markup.js'

const elements = (nodes) => nodes.filter((n) => n.type === 'element')
const text = (nodes) =>
  nodes
    .filter((n) => n.type === 'text')
    .map((n) => n.value)
    .join('')

describe('what counts as an envelope', () => {
  test('accepts hyphenated and underscored lowercase names', () => {
    for (const name of ['system-reminder', 'task_result', 'local-command-stdout', 'idea-plugin']) {
      expect(looksLikeEnvelope(name)).toBe(true)
    }
  })

  // These all appear in real tool output on this machine as source code.
  test('rejects the generics and tags that pollute tool output', () => {
    for (const name of ['T', 'String', 'Diagnostic', 'TClientContext', 'div', 'span', 'any', 'void', 'Value']) {
      expect(looksLikeEnvelope(name)).toBe(false)
    }
  })
})

describe('parsing', () => {
  test('plain text is returned untouched', () => {
    const nodes = parseMarkup('just a sentence, no markup')
    expect(hasMarkup(nodes)).toBe(false)
    expect(text(nodes)).toBe('just a sentence, no markup')
  })

  test('code containing generics stays plain text', () => {
    const code = 'fn f(v: Vec<String>) -> Option<T> { HashMap<K, V>::new() }'
    const nodes = parseMarkup(code)
    expect(hasMarkup(nodes)).toBe(false)
    expect(text(nodes)).toBe(code)
  })

  test('an envelope becomes an element and keeps surrounding text', () => {
    const nodes = parseMarkup('before\n<system-reminder>\nbe careful\n</system-reminder>\nafter')
    const els = elements(nodes)
    expect(els).toHaveLength(1)
    expect(els[0].name).toBe('system-reminder')
    expect(textOf(els[0])).toBe('be careful')
    expect(text(nodes)).toContain('before')
    expect(text(nodes)).toContain('after')
  })

  test('nested envelopes nest', () => {
    const nodes = parseMarkup(
      '<task-notification>\n<task-id>abc</task-id>\n<status>done</status>\n</task-notification>',
    )
    const outer = elements(nodes)[0]
    expect(outer.name).toBe('task-notification')
    const inner = elements(outer.children).map((c) => c.name)
    expect(inner).toEqual(['task-id', 'status'])
  })

  test('same-name nesting is balanced, not closed early', () => {
    const nodes = parseMarkup('<my-box>\n<my-box>inner</my-box>\ntail\n</my-box>')
    const outer = elements(nodes)[0]
    expect(outer.name).toBe('my-box')
    expect(textOf(outer)).toContain('inner')
    expect(textOf(outer)).toContain('tail')
    expect(elements(outer.children)).toHaveLength(1)
  })

  test('an unclosed tag is left as text rather than swallowing the rest', () => {
    const nodes = parseMarkup('<system-reminder>\nnever closed')
    expect(hasMarkup(nodes)).toBe(false)
    expect(text(nodes)).toBe('<system-reminder>\nnever closed')
  })

  test('a tag mid-line is not an envelope', () => {
    const nodes = parseMarkup('see <local-command-stdout>x</local-command-stdout> inline')
    expect(hasMarkup(nodes)).toBe(false)
  })

  test('self-closing tags are not envelopes', () => {
    expect(hasMarkup(parseMarkup('<my-thing/>'))).toBe(false)
  })

  test('attributes are captured', () => {
    const nodes = parseMarkup('<my-tag id="7" flag>body</my-tag>')
    const el = elements(nodes)[0]
    expect(el.attrs.id).toBe('7')
    expect(el.attrs).toHaveProperty('flag')
  })

  // A real plugin.xml appeared in tool output in these transcripts.
  test('a real XML document parses without choking on comments or CDATA', () => {
    const xml = [
      '<idea-plugin>',
      '    <!-- Unique plugin coordinate. -->',
      '    <id-tag>com.k8x.intellij</id-tag>',
      '    <description-tag><![CDATA[<p><b>k8x</b> support</p>]]></description-tag>',
      '</idea-plugin>',
    ].join('\n')
    const el = elements(parseMarkup(xml))[0]
    expect(el.name).toBe('idea-plugin')
    expect(elements(el.children).map((c) => c.name)).toEqual(['id-tag', 'description-tag'])
    expect(textOf(el)).toContain('com.k8x.intellij')
  })

  test('nothing is ever lost', () => {
    const source = 'a\n<task-notification>\n<task-id>1</task-id>\n</task-notification>\nb'
    const flatten = (nodes) =>
      nodes.map((n) => (n.type === 'text' ? n.value : flatten(n.children))).join('')
    expect(flatten(parseMarkup(source)).replace(/\s/g, '')).toBe(
      'a1b'.replace(/\s/g, ''),
    )
  })
})
