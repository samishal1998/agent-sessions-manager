// Screenshots the UI at desktop, tablet and phone widths and fails loudly on
// any console or page error — the checks a headless build cannot make.
//
//   bunx playwright install chromium     # once, to get the browser
//   asm serve --port 7455 &              # something to point at
//   SHOT_DIR=/tmp/shots bun run shots
//
// ASM_URL overrides the target, SHOT_DIR the output directory.
import { chromium } from 'playwright'
import { mkdirSync } from 'node:fs'

const BASE = process.env.ASM_URL ?? 'http://127.0.0.1:7455'
const OUT = process.env.SHOT_DIR ?? '/tmp/shots'
mkdirSync(OUT, { recursive: true })
const problems = []

const browser = await chromium.launch()

async function page(width, height) {
  const context = await browser.newContext({ viewport: { width, height }, deviceScaleFactor: 1 })
  const p = await context.newPage()
  p.on('console', (m) => {
    if (m.type() === 'error') problems.push(`[console ${width}px] ${m.text()}`)
  })
  p.on('pageerror', (e) => problems.push(`[pageerror ${width}px] ${e.message}`))
  await p.goto(BASE, { waitUntil: 'networkidle' })
  await p.waitForTimeout(700)
  return p
}

// Desktop: list, select menu open, transcript drawer, search results.
{
  const p = await page(1440, 900)
  await p.screenshot({ path: `${OUT}/01-desktop-list.png` })

  await p.click('.select-trigger')
  await p.waitForTimeout(250)
  await p.screenshot({ path: `${OUT}/02-desktop-multiselect.png` })
  // Pick both agents to prove multi-select stays open.
  const options = await p.$$('.select-option')
  for (const o of options) {
    await o.click()
    await p.waitForTimeout(120)
  }
  await p.screenshot({ path: `${OUT}/03-desktop-multiselect-both.png` })
  await p.keyboard.press('Escape')
  await p.waitForTimeout(200)

  // Hover an agent mark to show the tooltip.
  await p.hover('.row .agent-mark')
  await p.waitForTimeout(350)
  await p.screenshot({ path: `${OUT}/04-desktop-tooltip.png` })

  await p.click('.row-body')
  await p.waitForTimeout(2500)
  await p.screenshot({ path: `${OUT}/05-desktop-transcript.png` })
  await p.click('.drawer-head .icon-btn')
  await p.waitForTimeout(300)

  const inputs = await p.$$('.toolbar .field input')
  await inputs[1].fill('encoder')
  await inputs[1].press('Enter')
  await p.waitForTimeout(2500)
  await p.screenshot({ path: `${OUT}/06-desktop-search.png` })
  await p.close()
}

// Tablet: sidebar is an overlay.
{
  const p = await page(820, 1000)
  await p.screenshot({ path: `${OUT}/07-tablet-list.png` })
  await p.click('.toolbar .mobile-only')
  await p.waitForTimeout(350)
  await p.screenshot({ path: `${OUT}/08-tablet-sidebar.png` })
  await p.close()
}

// Phone: rows stack, actions wrap onto their own line.
{
  const p = await page(390, 844)
  await p.screenshot({ path: `${OUT}/09-phone-list.png`, fullPage: false })
  await p.click('.row-body')
  await p.waitForTimeout(2500)
  await p.screenshot({ path: `${OUT}/10-phone-transcript.png` })
  await p.close()
}

// Keyboard path through the custom select.
{
  const p = await page(1440, 900)
  await p.focus('.select-trigger')
  await p.keyboard.press('Enter')
  await p.waitForTimeout(200)
  await p.keyboard.press('ArrowDown')
  await p.keyboard.press('Enter')
  await p.waitForTimeout(200)
  const summary = await p.textContent('.select-trigger')
  problems.push(`[info] keyboard select summary: ${summary.trim()}`)
  await p.screenshot({ path: `${OUT}/11-keyboard-select.png` })
  await p.close()
}

await browser.close()
console.log(problems.length ? problems.join('\n') : 'no console or page errors')
