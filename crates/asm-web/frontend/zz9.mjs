import { chromium } from 'playwright'
const D='/tmp/claude-1000/-home-samimishal-projects-rust-agent-sessions-manager/fb779332-91dd-4d34-8d2f-12acf158c78f/scratchpad/audit-regress/'
const b=await chromium.launch(); const p=await b.newPage({viewport:{width:1500,height:1100}})
const log=[]; p.on('dialog',d=>{log.push('NATIVE DIALOG '+d.message());d.dismiss()})
p.on('console',m=>{if(['error','warning'].includes(m.type()))log.push('console.'+m.type()+': '+m.text())})
p.on('response',r=>{if(r.status()>=400)log.push('HTTP '+r.status()+' '+r.url())})
p.on('request',r=>{if(r.method()!=='GET')log.push('NONGET '+r.method()+' '+r.url())})
const step=async(n,f)=>{try{await f()}catch(e){console.log('FAIL',n,e.message.split('\n')[0])}}
await p.goto('http://localhost:7455'); await p.waitForTimeout(2500)
await step('sel all',async()=>{
 const all=p.locator('label:has-text("Select all shown"), :text("Select all shown")').first()
 await all.click(); await p.waitForTimeout(400)
 console.log('ticked',await p.locator('.s-row input[type=checkbox]:checked').count(),'of',await p.locator('.s-row').count(), (await p.locator('body').innerText()).match(/\d+ selected/)?.[0])
 await all.click(); await p.waitForTimeout(400); console.log('after toggle ticked',await p.locator('.s-row input[type=checkbox]:checked').count(), (await p.locator('body').innerText()).match(/\d+ selected/)?.[0])
 await all.click(); await p.waitForTimeout(400)
})
for (const name of ['Delete','Import','Move','Export']) await step(name,async()=>{
 await p.locator('.s-bulk, body').first().getByRole('button',{name,exact:true}).first().click({timeout:4000}); await p.waitForTimeout(600)
 const d=p.getByRole('dialog'); const n=await d.count()
 console.log(name,'dialogs',n, n? (await d.first().innerText()).replace(/\n+/g,' | ').slice(0,300):'')
 if(n){ await p.screenshot({path:D+'9-bulk-'+name+'.png'}); await p.keyboard.press('Escape'); await p.waitForTimeout(400); console.log('  esc ->',await p.getByRole('dialog').count(), 'selected still', (await p.locator('body').innerText()).match(/\d+ selected/)?.[0])}
})
await step('clear',async()=>{ await p.getByRole('button',{name:'Clear',exact:true}).click(); await p.waitForTimeout(400); console.log('after clear', (await p.locator('body').innerText()).match(/\d+ selected/)?.[0]??'bar gone', await p.locator('.s-row input[type=checkbox]:checked').count())})
console.log(log)
await b.close()
