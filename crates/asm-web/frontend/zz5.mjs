import { chromium } from 'playwright'
const D='/tmp/claude-1000/-home-samimishal-projects-rust-agent-sessions-manager/fb779332-91dd-4d34-8d2f-12acf158c78f/scratchpad/audit-regress/'
const b=await chromium.launch(); const p=await b.newPage({viewport:{width:1500,height:1100}})
const log=[]; p.on('dialog',d=>{log.push('NATIVE DIALOG '+d.message());d.dismiss()})
p.on('console',m=>{if(['error','warning'].includes(m.type()))log.push('console.'+m.type()+': '+m.text())})
p.on('pageerror',e=>log.push('pageerror '+e.message))
p.on('response',r=>{if(r.status()>=400)log.push('HTTP '+r.status()+' '+r.url())})
const step=async(n,f)=>{try{await f()}catch(e){console.log('FAIL',n,e.message.split('\n')[0])}}
await p.goto('http://localhost:7455'); await p.waitForTimeout(2500)
await step('tsearch',async()=>{
 await p.locator('.s-row').first().getByRole('button').first().click(); await p.waitForTimeout(1200)
 const s=p.getByRole('searchbox',{name:/Search this transcript/}).or(p.getByPlaceholder('Search...'))
 await s.first().fill('contrast'); await s.first().press('Enter'); await p.waitForTimeout(800)
 await p.screenshot({path:D+'6-tsearch.png'}); console.log('marks',await p.locator('mark').count())
 console.log('earlier btn',await p.getByText(/Show \d+ earlier/).count())
 await p.keyboard.press('Escape'); await p.waitForTimeout(500)
 console.log('reply visible after esc',await p.getByText('Reply in this').count())
 await p.locator('.s-row').first().getByRole('button').first().click(); await p.waitForTimeout(800)
 await p.getByPlaceholder(/Reply in this/).focus(); await p.keyboard.press('Escape'); await p.waitForTimeout(500)
 console.log('reply visible after esc from composer',await p.getByPlaceholder(/Reply in this/).count())
})
// row action names
await step('actions',async()=>{
 const r=p.locator('.s-row').nth(4)
 console.log(await r.getByRole('button').evaluateAll(e=>e.map(x=>(x.getAttribute('aria-label')||x.innerText)+(x.disabled?' [dis]':''))))
 console.log(await r.getByRole('link').evaluateAll(e=>e.map(x=>x.getAttribute('aria-label')+' '+x.href)))
})
console.log(log)
await b.close()
