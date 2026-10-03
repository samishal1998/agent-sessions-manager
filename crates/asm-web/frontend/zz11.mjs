import { chromium } from 'playwright'
const D='/tmp/claude-1000/-home-samimishal-projects-rust-agent-sessions-manager/fb779332-91dd-4d34-8d2f-12acf158c78f/scratchpad/audit-regress/'
const b=await chromium.launch(); const ctx=await b.newContext({viewport:{width:1500,height:1100},permissions:['clipboard-read','clipboard-write']}); const p=await ctx.newPage()
const log=[]; p.on('dialog',d=>{log.push('NATIVE DIALOG '+d.message());d.dismiss()})
p.on('console',m=>{if(['error','warning'].includes(m.type()))log.push('console.'+m.type()+': '+m.text())})
p.on('response',r=>{if(r.status()>=400)log.push('HTTP '+r.status()+' '+r.url())})
p.on('request',r=>{if(r.method()!=='GET')log.push('NONGET '+r.method()+' '+r.url())})
const step=async(n,f)=>{try{await f()}catch(e){console.log('FAIL',n,e.message.split('\n')[0])}}
const rows=()=>p.locator('tbody tr').count()
await p.goto('http://localhost:7455'); await p.waitForTimeout(2500)
await p.getByRole('link',{name:/^Hub/}).or(p.getByRole('button',{name:/^Hub/})).first().click(); await p.waitForTimeout(1200)
console.log('rows',await rows(),p.url())
await step('chips',async()=>{
 await p.getByText(/^Synced \(4\)/).last().click(); await p.waitForTimeout(300); console.log('synced rows',await rows())
 await p.getByText(/^Synced \(4\)/).last().click(); 
 await p.getByRole('switch',{name:/Only what needs/}).click(); await p.waitForTimeout(300); console.log('needs doing rows',await rows())
 await p.getByRole('switch',{name:/Only what needs/}).click()
 await p.getByLabel('Filter hub sessions').fill('pool'); await p.waitForTimeout(300); console.log('text pool rows',await rows())
 await p.getByLabel('Filter hub sessions').fill('')
})
await step('sheet',async()=>{
 await p.getByRole("button",{name:/^Details for/}).first().click(); await p.waitForTimeout(800); await p.screenshot({path:D+'11-sheet.png'})
 const d=p.getByRole('dialog'); console.log('dialogs',await d.count()); console.log((await d.first().innerText()).replace(/\n+/g,' | '))
 console.log('buttons',await d.first().getByRole('button').allInnerTexts())
 console.log('inputs',await d.first().locator('input,textarea').evaluateAll(e=>e.map(x=>x.value+' ro='+x.readOnly)))
 await p.keyboard.press('Escape'); await p.waitForTimeout(500); console.log('after esc',await d.count())
 await p.getByRole("button",{name:/^Details for/}).nth(1).click(); await p.waitForTimeout(800);console.log((await d.first().innerText()).replace(/\n+/g,' | ')); await p.screenshot({path:D+'11-sheet2.png'}); await p.keyboard.press('Escape')
 await p.getByRole("button",{name:/^Details for/}).nth(5).click(); await p.waitForTimeout(800);console.log((await d.first().innerText()).replace(/\n+/g,' | ')); await p.screenshot({path:D+'11-sheet3.png'});console.log('inputs',await d.first().locator('input,textarea').evaluateAll(e=>e.map(x=>x.value+' ro='+x.readOnly)));
 const cp=d.first().getByRole('button',{name:/Copy/}); if(await cp.count()){await cp.first().click(); await p.waitForTimeout(400); console.log('clip',await p.evaluate(()=>navigator.clipboard.readText()))}
 await p.keyboard.press('Escape')
})
console.log(log)
await b.close()
