import { chromium } from 'playwright'
const D='/tmp/claude-1000/-home-samimishal-projects-rust-agent-sessions-manager/fb779332-91dd-4d34-8d2f-12acf158c78f/scratchpad/audit-regress/'
const b=await chromium.launch(); const ctx=await b.newContext({viewport:{width:390,height:844},hasTouch:true,isMobile:true}); const p=await ctx.newPage()
const log=[]; p.on('dialog',d=>{log.push('NATIVE DIALOG '+d.message());d.dismiss()})
p.on('console',m=>{if(['error','warning'].includes(m.type()))log.push('console.'+m.type()+': '+m.text())})
p.on('response',r=>{if(r.status()>=400)log.push('HTTP '+r.status()+' '+r.url())})
const step=async(n,f)=>{try{await f()}catch(e){console.log('FAIL',n,e.message.split('\n')[0])}}
await p.goto('http://localhost:7455'); await p.waitForTimeout(2500)
console.log('hscroll',await p.evaluate(()=>[document.documentElement.scrollWidth,innerWidth]))
await p.screenshot({path:D+'13-narrow.png'})
console.log('buttons top',await p.locator('header button, [aria-label*=enu]').evaluateAll(e=>e.map(x=>x.getAttribute('aria-label')+'|'+x.innerText)))
await step('menu',async()=>{
 await p.getByRole('button',{name:/menu|navigation/i}).first().click(); await p.waitForTimeout(600); await p.screenshot({path:D+'13-menu.png'})
 await p.getByRole('dialog').getByText('Hub',{exact:true}).click(); await p.waitForTimeout(1000); await p.screenshot({path:D+'13-hub.png'}); console.log('hub hscroll',await p.evaluate(()=>[document.documentElement.scrollWidth,innerWidth]), (await p.locator('h1').allInnerTexts()))
})
await step('row',async()=>{
 await p.getByRole('button',{name:/menu|navigation/i}).first().click(); await p.waitForTimeout(500); await p.getByRole('dialog').getByText('Sessions',{exact:true}).click(); await p.waitForTimeout(800)
 await p.locator('.s-row').first().scrollIntoViewIfNeeded(); await p.screenshot({path:D+'13-rows.png'})
 const r=p.locator('.s-row').first().boundingBox(); console.log('row box',r)
 console.log('actions visible',await p.locator('.s-row').first().getByRole('button').evaluateAll(e=>e.map(x=>{const r=x.getBoundingClientRect();return (x.getAttribute('aria-label')||'row')+' '+Math.round(r.x)+','+Math.round(r.width)+'x'+Math.round(r.height)})))
 await p.locator('.s-row').first().getByRole('button').first().click(); await p.waitForTimeout(1000); await p.screenshot({path:D+'13-drawer.png'})
})
console.log(log)
await b.close()
