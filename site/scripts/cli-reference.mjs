// Writes the command-line reference from the real binary's `--help`, so the
// docs cannot say a flag exists that the code does not have, or miss one it
// does. Run before every dev/build (see package.json); the output is not
// committed.
//
//   ASM_BIN=/path/to/asm node scripts/cli-reference.mjs
import { execFileSync } from 'node:child_process'
import { existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const out = resolve(here, '../src/content/docs/reference/cli')
const candidates = [
  process.env.ASM_BIN,
  resolve(here, '../../target/release/asm'),
  resolve(here, '../../target/debug/asm'),
].filter(Boolean)
const bin = candidates.find((p) => existsSync(p))
if (!bin) {
  console.error(
    'cli-reference: no asm binary found. Build one (`cargo build`) or set ASM_BIN.\n  looked in: ' +
      candidates.join(', '),
  )
  process.exit(1)
}

const help = (...args) =>
  execFileSync(bin, [...args, '--help'], {
    encoding: 'utf8',
    env: { ...process.env, NO_COLOR: '1', COLUMNS: '1000' },
  })
const version = execFileSync(bin, ['--version'], { encoding: 'utf8' }).trim()

/** Split a clap help text into its intro, usage, and named sections. */
function parse(text) {
  const lines = text.replace(/\r/g, '').split('\n')
  const intro = []
  const sections = {}
  let usage = ''
  let current = null
  for (const line of lines) {
    if (line.startsWith('Usage:')) {
      usage = line.slice('Usage:'.length).trim()
      current = null
    } else if (/^[A-Z][A-Za-z ]*:$/.test(line)) {
      current = line.slice(0, -1)
      sections[current] = []
    } else if (current) {
      sections[current].push(line)
    } else if (!usage) {
      intro.push(line)
    }
  }
  return { intro: intro.join('\n').trim(), usage, sections }
}

/** `  -x, --long <VALUE>  Description` entries; wrapped lines fold into the last. */
function entries(rows, kind) {
  const found = []
  for (const row of rows ?? []) {
    if (!row.trim()) continue
    const m =
      kind === 'option'
        ? // Long-only flags are indented past the short-flag column, so the
          // leading space is any amount, not two.
          row.match(/^\s+((?:-\w, )?--[\w-]+(?:[ =](?:<[^>]+>|\[[^\]]+\]))?)\s{2,}(.*)$/) ||
          row.match(/^\s+(-\w(?:, --[\w-]+)?(?:[ =]<[^>]+>)?)\s{2,}(.*)$/)
        : row.match(/^\s{2}(\S.*?)\s{2,}(.*)$/)
    if (m) found.push({ sig: m[1].trim(), desc: m[2].trim() })
    else if (found.length) found[found.length - 1].desc += ' ' + row.trim()
  }
  return found
}

// HTML and table syntax are hazards in a .md file: clap writes `<REF>` and
// `[REFS]...`, and a stray `|` splits a table cell. Code spans keep their
// content but still need the pipe escaped.
const cell = (s) =>
  s
    .split(/(`[^`]*`)/)
    .map((part) =>
      part.startsWith('`') && part.endsWith('`')
        ? part.replace(/\|/g, '\\|')
        : part.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/\|/g, '\\|'),
    )
    .join('')
const code = (s) => '`' + s.replace(/\|/g, '\\|') + '`'
const first = (s) => s.split(/(?<=[.!?])\s/)[0].replace(/\s+/g, ' ').trim()

const top = parse(help())
const globals = new Set(entries(top.sections.Options, 'option').map((o) => o.sig.match(/--[\w-]+/)?.[0]))
globals.delete(undefined)

const table = (rows, head) =>
  rows.length
    ? `| ${head[0]} | ${head[1]} |\n|---|---|\n` +
      rows.map((r) => `| ${code(r.sig)} | ${cell(r.desc)} |`).join('\n') +
      '\n'
    : ''

/** One command (and, recursively, its subcommands) as Markdown sections. */
function render(path, level) {
  const p = parse(help(...path.slice(1)))
  const name = path.join(' ')
  const hashes = '#'.repeat(level)
  let md = ''
  if (level > 1) md += `${hashes} \`${name}\`\n\n`
  md += (p.intro ? cell(p.intro).replace(/\n/g, '\n\n') : '') + '\n\n'
  md += '```text\n' + `${name} ${p.usage.replace(/^asm[^\s]*(\s[a-z-]+)*\s?/, '')}`.trim() + '\n```\n\n'
  const args = entries(p.sections.Arguments, 'arg')
  if (args.length) md += `${hashes}# Arguments\n\n` + table(args, ['Argument', 'Meaning']) + '\n'
  const opts = entries(p.sections.Options, 'option').filter((o) => {
    const long = o.sig.match(/--[\w-]+/)?.[0]
    return !globals.has(long) && long !== '--help' && long !== '--version'
  })
  if (opts.length) md += `${hashes}# Options\n\n` + table(opts, ['Option', 'Meaning']) + '\n'
  const subs = entries(p.sections.Commands, 'cmd').filter((c) => c.sig !== 'help')
  for (const sub of subs) md += render([...path, sub.sig], level + 1)
  return md
}

// `asm index` would land on `index.md`, which Astro serves as the directory
// itself and would collide with the overview.
const file = (sig) => (sig === 'index' ? 'index-command' : sig)

rmSync(out, { recursive: true, force: true })
mkdirSync(out, { recursive: true })
const frontmatter = (title, description, order) =>
  `---\ntitle: ${JSON.stringify(title)}\ndescription: ${JSON.stringify(description)}\neditUrl: false\nsidebar:\n  order: ${order}\n---\n\n`

const commands = entries(top.sections.Commands, 'cmd').filter((c) => c.sig !== 'help')
const globalRows = entries(top.sections.Options, 'option').filter((o) => !/--help|--version/.test(o.sig))

let overview = frontmatter('Command line overview', 'Every asm command, and the options they all share.', 0)
overview += `Generated from \`${version}\`. Every command is also available as \`asm <command> --help\`.\n\n`
overview += `Run \`asm\` with no command on a terminal to open the [terminal UI](/guides/tui/); piped, it prints the session table.\n\n`
overview += '## Options every command accepts\n\n' + table(globalRows, ['Option', 'Meaning']) + '\n'
overview +=
  'These narrow what a command acts on. `--agent` and `--project` filter the sessions; `--include-children` ' +
  'adds subagent sessions, which are hidden by default. On `push` and `pull`, `--all` means everything the ' +
  'filters match.\n\n'
overview += '## Commands\n\n| Command | What it does |\n|---|---|\n'
for (const c of commands) overview += `| [${code('asm ' + c.sig)}](/reference/cli/${file(c.sig)}/) | ${cell(first(c.desc))} |\n`
writeFileSync(resolve(out, 'overview.md'), overview)

commands.forEach((c, i) => {
  const body = render(['asm', c.sig], 1)
  writeFileSync(
    resolve(out, `${file(c.sig)}.md`),
    frontmatter(`asm ${c.sig}`, first(c.desc), i + 1) + body,
  )
})
console.log(`cli-reference: ${commands.length} commands from ${bin} (${version})`)
