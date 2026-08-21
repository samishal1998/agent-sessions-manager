import { chromium } from 'playwright'
const OUT = process.argv[2]
const PORT = process.argv[3] || '7455'
const b = await chromium.launch()
const errors = []
async function page(width, height) {
  const p = await b.newPage({ viewport: { width, height }, deviceScaleFactor: 2 })
  p.on('console', (m) => m.type() === 'error' && errors.push(m.text()))
  p.on('pageerror', (e) => errors.push(e.message))
  await p.goto(`http://127.0.0.1:${PORT}/`, { waitUntil: 'networkidle' })
  await p.waitForSelector('.row, [role="button"]', { timeout: 15000 })
  return p
}
async function shot(name, width, height, prep) {
  const p = await page(width, height)
  if (prep) await prep(p)
  await p.waitForTimeout(700)
  await p.screenshot({ path: `${OUT}/${name}.png` })
  await p.close()
}
async function openRow(p, needle) {
  const rows = p.locator('[role="button"]')
  const n = await rows.count()
  for (let i = 0; i < n; i++) {
    const t = await rows.nth(i).innerText().catch(() => '')
    if (t.includes(needle)) {
      await rows.nth(i).click()
      return
    }
  }
  await rows.first().click()
}

await shot('list', 1440, 900)
await shot('transcript', 1440, 900, (p) => openRow(p, 'connection-pool'))
await shot('bulk', 1440, 900, async (p) => {
  for (const i of [0, 1, 2]) await p.locator('.row .tick').nth(i).click()
})
await shot('mobile', 420, 860)

// A real turn, not a mock-up: the composer sends into the demo store's own
// Claude session and the reply is captured while it is still in flight.
// Everything in that session is fabricated, so nothing real is on screen.
{
  const p = await page(1440, 900)
  await openRow(p, 'connection-pool')
  await p.waitForSelector('.composer-input', { timeout: 15000 })
  await p.locator('.composer-input').fill('Add the Drop guard and run the pool tests.')
  await p.locator('.composer button[type=submit]').click()
  await p.waitForSelector('.message.live', { timeout: 20000 })
  // Wait for the agent to actually say something, so the shot shows a reply
  // arriving rather than an empty in-flight box.
  await p
    .waitForFunction(
      () => (document.querySelector('.message.live')?.innerText || '').length > 260,
      { timeout: 180000 },
    )
    .catch(() => {})
  await p.screenshot({ path: `${OUT}/reply.png` })
  await p.close()
}

await b.close()
if (errors.length) {
  console.error('CONSOLE ERRORS:\n' + errors.join('\n'))
  process.exit(1)
}
console.log('shots ok')
