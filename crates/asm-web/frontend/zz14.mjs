import { chromium } from 'playwright'
import fs from 'fs'
const D='/tmp/claude-1000/-home-samimishal-projects-rust-agent-sessions-manager/fb779332-91dd-4d34-8d2f-12acf158c78f/scratchpad/audit-regress/'
const tok=fs.readFileSync('/tmp/claude-1000/-home-samimishal-projects-rust-agent-sessions-manager/fb779332-91dd-4d34-8d2f-12acf158c78f/scratchpad/dadmin.tok','utf8').trim()
const b=await chromium.launch(); const ctx=await b.newContext({viewport:{width:1400,height:1000},permissions:['clipboard-read','clipboard-write']}); const p=await ctx.newPage()
const log=[]; p.on('dialog',d=>{log.push('NATIVE DIALOG '+d.message());d.dismiss()})
p.on('console',m=>{if(['error','warning'].includes(m.type()))log.push('console.'+m.type()+': '+m.text())})
p.on('response',r=>{if(r.status()>=400)log.push('HTTP '+r.status()+' '+r.url())})
p.on('request',r=>{if(r.method()!=='GET')log.push('NONGET '+r.method()+' '+r.url())})
const step=async(n,f)=>{try{await f()}catch(e){console.log('FAIL',n,e.message.split('\n')[0])}}
await p.goto('http://localhost:7481/admin'); await p.waitForTimeout(1500)
await p.screenshot({path:D+'14-login.png'})
console.log((await p.locator('body').innerText()).slice(0,500))
await step('wrong',async()=>{ const i=p.locator('input').first(); await i.fill('wrong-token-xyz'); await i.press('Enter'); await p.waitForTimeout(1200); await p.screenshot({path:D+'14-wrong.png'}); console.log('wrong ->',(await p.locator('body').innerText()).replace(/\n+/g,' | ').slice(0,400)) })
await step('right',async()=>{ const i=p.locator('input').first(); await i.fill(tok); await i.press('Enter'); await p.waitForTimeout(2000); await p.screenshot({path:D+'14-in.png'}); const t=await p.locator('body').innerText(); console.log('in ->',t.replace(/\n+/g,' | ').slice(0,1800)); console.log('tok leaked in text?',t.includes(tok)) })


const dlg=async(label,btn)=>{ await btn.click({timeout:4000}); await p.waitForTimeout(500); const d=p.getByRole('dialog'); const n=await d.count(); console.log(label,'dialog',n,n?(await d.first().innerText()).replace(/\n+/g,' | ').slice(0,260):''); if(n){ await p.getByRole('button',{name:'Cancel'}).click(); await p.waitForTimeout(400); console.log('  cancel ->',await p.getByRole('dialog').count())} }
await step('revoke',async()=>{ await p.getByRole('tab',{name:/^Machines/}).click(); await p.waitForTimeout(500); await dlg('Revoke',p.getByRole('button',{name:/^Revoke/}).first()); console.log('rows',await p.locator('tbody tr:visible').count()) })
await step('delete',async()=>{ await p.getByRole('tab',{name:/^Sessions/}).click(); await p.waitForTimeout(500); await dlg('Delete',p.getByRole('button',{name:/^Delete/}).first()); console.log('rows',await p.locator('tbody tr:visible').count()); await p.getByLabel('Filter sessions').fill('pool'); await p.waitForTimeout(300); console.log('filtered rows',await p.locator('tbody tr:visible').count()) })
await step('storage',async()=>{ await p.getByRole('tab',{name:/^Storage/}).click(); await p.waitForTimeout(600); await p.getByRole('button',{name:'Preview'}).click(); await p.waitForTimeout(1200); await p.screenshot({path:D+'14-storage.png'}); const t=await p.locator('body').innerText(); console.log(t.slice(t.indexOf('Storage',40)).replace(/\n+/g,' | ').slice(0,600)); console.log(await p.getByRole('button').evaluateAll(e=>e.map(x=>x.innerText).filter(Boolean)));
 const c=p.getByRole('button',{name:/^Collect/}); if(await c.count()) await dlg('Collect',c.first()) })
await step('activity',async()=>{ await p.getByRole('tab',{name:/^Activity/}).click(); await p.waitForTimeout(600); await p.screenshot({path:D+'14-activity.png'}) })
await step('signout',async()=>{ await p.getByRole('button',{name:'Sign out'}).click(); await p.waitForTimeout(800); console.log('after signout',(await p.locator('body').innerText()).slice(0,200).replace(/\n+/g,' | ')); await p.reload(); await p.waitForTimeout(1000); console.log('reload still signed out?',await p.getByText('Admin token').count()) })
console.log(log)
await b.close()
