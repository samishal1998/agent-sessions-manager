import { chromium } from 'playwright'
const D='/tmp/claude-1000/-home-samimishal-projects-rust-agent-sessions-manager/fb779332-91dd-4d34-8d2f-12acf158c78f/scratchpad/audit-regress/'
const b=await chromium.launch(); const ctx=await b.newContext({viewport:{width:1500,height:1100}}); const p=await ctx.newPage()
const log=[]; p.on('dialog',d=>{log.push('NATIVE DIALOG '+d.message());d.dismiss()})
p.on('console',m=>{if(['error','warning'].includes(m.type()))log.push('console.'+m.type()+': '+m.text())})
p.on('response',r=>{if(r.status()>=400)log.push('HTTP '+r.status()+' '+r.url())})
p.on('request',r=>{if(r.method()!=='GET')log.push('NONGET '+r.method()+' '+r.url())})
const step=async(n,f)=>{try{await f()}catch(e){console.log('FAIL',n,e.message.split('\n')[0])}}
const rows=()=>p.locator('tbody tr').count()
await p.goto('http://localhost:7455'); await p.waitForTimeout(2500)
await p.getByRole('button',{name:/Hub view/}).click(); await p.waitForTimeout(1200)
await p.screenshot({path:D+'12-hub.png',fullPage:true})
console.log('chip els',await p.getByText(/^Synced \(\d\)/).evaluateAll(e=>e.map(x=>x.tagName+'.'+x.className+' vis='+(x.offsetParent!==null))))
await step('chips',async()=>{
 const c=p.getByRole('checkbox',{name:/^Synced/}).or(p.getByRole('button',{name:/^Synced/})).or(p.locator('button:has-text("Synced (")')).last()
 await c.click({timeout:4000}); await p.waitForTimeout(300); console.log('synced rows',await rows()); await c.click()
})
await step('only',async()=>{ const s=p.getByRole('switch',{name:/Only what needs/}); await s.click(); await p.waitForTimeout(300); console.log('needs rows',await rows()); await s.click() })
await step('text',async()=>{ await p.getByLabel('Filter hub sessions').fill('pool'); await p.waitForTimeout(300); console.log('pool rows',await rows()); await p.getByLabel('Filter hub sessions').fill('zzzz'); await p.waitForTimeout(300); console.log('empty:',(await p.locator('body').innerText()).match(/No .{0,80}/)?.[0]); await p.screenshot({path:D+'12-empty.png'}); await p.getByLabel('Filter hub sessions').fill('')})
await step('hdr',async()=>{ for(const n of ['Check now','Push','Pull']) { const bt=p.getByRole('button',{name:new RegExp('^'+n)}).first(); console.log(n,'disabled',await bt.isDisabled(), await bt.innerText()) } })
await step('sheetclose',async()=>{ await p.getByRole('button',{name:/^Details for/}).first().click(); await p.waitForTimeout(600); console.log(await p.getByRole('dialog').first().getByRole('button').evaluateAll(e=>e.map(x=>x.getAttribute('aria-label')+'|'+x.innerText))) })
console.log(log)
await b.close()
