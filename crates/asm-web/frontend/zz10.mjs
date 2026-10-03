import { chromium } from 'playwright'
const D='/tmp/claude-1000/-home-samimishal-projects-rust-agent-sessions-manager/fb779332-91dd-4d34-8d2f-12acf158c78f/scratchpad/audit-regress/'
const b=await chromium.launch(); const ctx=await b.newContext({viewport:{width:1500,height:1100},permissions:['clipboard-read','clipboard-write']}); const p=await ctx.newPage()
const log=[]; p.on('dialog',d=>{log.push('NATIVE DIALOG '+d.message());d.dismiss()})
p.on('console',m=>{if(['error','warning'].includes(m.type()))log.push('console.'+m.type()+': '+m.text())})
p.on('response',r=>{if(r.status()>=400)log.push('HTTP '+r.status()+' '+r.url())})
p.on('request',r=>{if(r.method()!=='GET')log.push('NONGET '+r.method()+' '+r.url())})
const step=async(n,f)=>{try{await f()}catch(e){console.log('FAIL',n,e.message.split('\n')[0])}}
await p.goto('http://localhost:7455'); await p.waitForTimeout(2500)
await step('hubview',async()=>{ await p.getByRole('button',{name:/Hub view/}).click(); await p.waitForTimeout(1500); console.log('url',p.url()); await p.screenshot({path:D+'10-hub.png'}); console.log((await p.locator('body').innerText()).slice(0,2500)) })
console.log(log)
await b.close()
