import { chromium } from 'playwright'
const D='/tmp/claude-1000/-home-samimishal-projects-rust-agent-sessions-manager/fb779332-91dd-4d34-8d2f-12acf158c78f/scratchpad/audit-regress/'
const b=await chromium.launch(); const ctx=await b.newContext({viewport:{width:1500,height:1100}}); const p=await ctx.newPage()
const log=[]; p.on('dialog',d=>{log.push('NATIVE DIALOG '+d.message());d.dismiss()})
p.on('console',m=>{if(['error','warning'].includes(m.type()))log.push('console.'+m.type()+': '+m.text())})
p.on('response',r=>{if(r.status()>=400)log.push('HTTP '+r.status()+' '+r.url())})
p.on('request',r=>{if(r.method()!=='GET')log.push('NONGET '+r.method()+' '+r.url())})
const step=async(n,f)=>{try{await f()}catch(e){console.log('FAIL',n,e.message.split('\n')[0])}}
await p.goto('http://localhost:7455'); await p.waitForTimeout(2500)
console.log('rows',await p.locator('.s-row').count())
const row=p.locator('.s-row').filter({hasText:'Port the retry'})
for (const name of ['Move to another project','Import into the other agent','Delete']) await step(name,async()=>{
 await row.getByRole('button',{name:new RegExp('^'+name)}).first().click({timeout:4000}); await p.waitForTimeout(700)
 const d=p.getByRole('dialog'); const n=await d.count()
 console.log(name,'-> dialogs',n, n? (await d.first().innerText()).replace(/\n+/g,' | ').slice(0,300):'')
 if(n){await p.screenshot({path:D+'7-'+name.split(' ')[0]+'.png'}); await p.getByRole('button',{name:'Cancel'}).click(); await p.waitForTimeout(400); console.log('  after cancel',await p.getByRole('dialog').count())}
})
// bulk
await step('bulk',async()=>{
 await p.getByLabel('Select all shown').check?.().catch(()=>{}); 
 await p.getByText('Select all shown').click(); await p.waitForTimeout(500)
 await p.screenshot({path:D+'8-bulk.png'})
 console.log('bar', (await p.locator('body').innerText()).match(/\d+ selected[\s\S]{0,200}/)?.[0])
 console.log('bulk buttons', await p.getByRole('toolbar').getByRole('button').allInnerTexts().catch(()=>'no toolbar'))
})
console.log(log)
await b.close()
