import { chromium } from 'playwright'
const D='/tmp/claude-1000/-home-samimishal-projects-rust-agent-sessions-manager/fb779332-91dd-4d34-8d2f-12acf158c78f/scratchpad/audit-regress/'
const b=await chromium.launch(); const p=await b.newPage({viewport:{width:1500,height:1100}})
const log=[]; p.on('dialog',d=>{log.push('NATIVE DIALOG '+d.message());d.dismiss()})
p.on('console',m=>{if(['error','warning'].includes(m.type()))log.push('console.'+m.type()+': '+m.text())})
p.on('response',r=>{if(r.status()>=400)log.push('HTTP '+r.status()+' '+r.url())})
p.on('request',r=>{if(r.method()!=='GET')log.push('NONGET '+r.method()+' '+r.url())})
const step=async(n,f)=>{try{await f()}catch(e){console.log('FAIL',n,e.message.split('\n')[0])}}
await p.goto('http://localhost:7455'); await p.waitForTimeout(2500)
await step('bulk',async()=>{
 await p.locator('.s-row').nth(1).getByRole('checkbox').check({force:true}); await p.locator('.s-row').nth(2).getByRole('checkbox').check({force:true}); await p.waitForTimeout(500)
 await p.screenshot({path:D+'8-bulk.png'})
 console.log(await p.getByRole('checkbox',{name:/Select all/}).count())
 const t=await p.locator('body').innerText(); console.log(t.match(/.{0,100}selected.{0,300}/s)?.[0])
})
console.log(log)
await b.close()
