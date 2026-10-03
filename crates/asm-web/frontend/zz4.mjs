import { chromium } from 'playwright'
const D='/tmp/claude-1000/-home-samimishal-projects-rust-agent-sessions-manager/fb779332-91dd-4d34-8d2f-12acf158c78f/scratchpad/audit-regress/'
const b=await chromium.launch(); const p=await b.newPage({viewport:{width:1400,height:1000}})
const log=[]; p.on('dialog',d=>{log.push('NATIVE DIALOG '+d.message());d.dismiss()})
p.on('console',m=>{if(['error','warning'].includes(m.type()))log.push('console.'+m.type()+': '+m.text())})
p.on('pageerror',e=>log.push('pageerror '+e.message))
p.on('response',r=>{if(r.status()>=400)log.push('HTTP '+r.status()+' '+r.url())})
const step=async(n,f)=>{try{await f()}catch(e){console.log('FAIL',n,e.message.split('\n')[0])}}
await p.goto('http://localhost:7455'); await p.waitForTimeout(2500)
await step('search',async()=>{
 const f=p.getByLabel('Search inside transcripts'); await f.fill('pool'); await f.press('Enter'); await p.waitForTimeout(2500)
 await p.screenshot({path:D+'4-search.png'})
 console.log('marks',await p.locator('mark').count(), await p.locator('mark').first().innerText().catch(()=>''))
 console.log((await p.locator('body').innerText()).match(/Back to sessions[\s\S]{0,700}/)?.[0])
 await p.getByRole('button',{name:/Back to sessions/}).click(); await p.waitForTimeout(500); console.log('rows back',await p.locator('.s-row').count(), 'ft value',await f.inputValue())
 await f.fill('pool'); await f.press('Enter'); await p.waitForTimeout(1500); await f.press('Escape'); await p.waitForTimeout(500); console.log('esc rows',await p.locator('.s-row').count())
})
await step('open',async()=>{
 await p.locator('.s-row button, .s-row').first().click({position:{x:200,y:20}}); await p.waitForTimeout(1500)
 await p.screenshot({path:D+'5-drawer.png'})
 console.log('dialogs',await p.locator('[role=dialog]').count())
 console.log((await p.locator('[role=dialog]').first().innerText()).slice(0,1500))
})
console.log(log)
await b.close()
