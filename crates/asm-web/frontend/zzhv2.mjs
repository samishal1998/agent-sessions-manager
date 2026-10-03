import {chromium} from 'playwright'
const b=await chromium.launch();const p=await b.newPage();await p.setViewportSize({width:1440,height:900});await p.goto('http://localhost:5212/');await p.waitForTimeout(800)
const n=await p.$$('[href*=hub], button:has-text("Hub")');await n[0].click();await p.waitForTimeout(800)
console.log(await p.evaluate(()=>[...document.querySelectorAll('h1')].map(h=>h.outerHTML.slice(0,160)+' | '+h.parentElement.className)))
await b.close()
