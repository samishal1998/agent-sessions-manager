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

// Desktop: list, agent filters, transcript drawer, search results.
{
  const p = await page(1440, 900)
  await p.screenshot({ path: `${OUT}/01-desktop-list.png` })

  const chips = await p.$$('.chip')
  await chips[0].click()
  await p.waitForTimeout(250)
  await p.screenshot({ path: `${OUT}/02-desktop-multiselect.png` })
  // A second agent, without turning the first one off.
  await chips[1].click()
  await p.waitForTimeout(250)
  const on = await p.$$eval('.chip[aria-pressed="true"]', (els) => els.length)
  if (on !== 2) problems.push(`[filter] expected two agents chosen, got ${on}`)
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

// Keyboard path through the filters: tab to an agent chip, toggle it.
{
  const p = await page(1440, 900)
  // Tab to the chips rather than focusing them by hand: their place in the
  // tab order is part of what is being checked.
  for (let i = 0; i < 40; i++) {
    await p.keyboard.press('Tab')
    if (await p.evaluate(() => document.activeElement?.classList.contains('chip'))) break
  }
  if (!(await p.evaluate(() => document.activeElement?.classList.contains('chip')))) {
    problems.push('[keyboard] no agent chip is reachable with Tab')
  }
  await p.keyboard.press('Enter')
  await p.waitForTimeout(200)
  const pressed = await p.evaluate(() => document.activeElement?.getAttribute('aria-pressed'))
  if (pressed !== 'true') problems.push(`[keyboard] agent chip did not toggle (aria-pressed=${pressed})`)
  await p.screenshot({ path: `${OUT}/11-keyboard-filter.png` })
  await p.close()
}

await browser.close()
console.log(problems.length ? problems.join('\n') : 'no console or page errors')
