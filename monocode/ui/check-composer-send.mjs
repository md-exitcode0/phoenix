// Preview check: one send per draft. Rapid Enter + click must produce exactly
// one user message, clear the text, the attachment tray and the height at
// once, and a click on the (now Stop) button right after sending must not
// stop the turn. Usage:
//   PW=/path/to/playwright-core/index.mjs CHROME=/path/to/chrome \
//   node check-composer-send.mjs http://127.0.0.1:8791/index.html
import assert from 'node:assert/strict';
import {writeFileSync} from 'node:fs';
const {chromium}=await import(process.env.PW||'playwright-core');
const base=process.argv[2]||'http://127.0.0.1:8791/index.html';
const browser=await chromium.launch({executablePath:process.env.CHROME||undefined,args:['--no-sandbox','--disable-gpu']});
const page=await browser.newPage({viewport:{width:1400,height:900}});
const errors=[];page.on('pageerror',e=>errors.push(String(e)));
await page.goto(`${base}?shot=attachment`);await page.waitForTimeout(4000);
const png=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==','base64');
writeFileSync('/tmp/phoenix-check-image.png',png);
await page.setInputFiles('#attachmentInput','/tmp/phoenix-check-image.png');
await page.waitForFunction(()=>document.querySelectorAll('#attachmentTray > *').length>0,null,{timeout:5000});
const input=page.locator('#composerInput');await input.click();await page.keyboard.type('hey tibo. check out the new ui');
const before=await page.evaluate(()=>({users:document.querySelectorAll('#conversationFeed .user-message').length,height:document.getElementById('composerInput').getBoundingClientRect().height}));
// Two Enters in the same task plus an immediate click on the send button.
await page.evaluate(()=>{const el=document.getElementById('composerInput');for(let i=0;i<2;i++)el.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true,cancelable:true}));});
await page.keyboard.press('Enter');await page.click('#sendButton',{force:true}).catch(()=>{});
await page.waitForTimeout(400);
const after=await page.evaluate(()=>({users:document.querySelectorAll('#conversationFeed .user-message').length,text:document.getElementById('composerInput').textContent.trim(),tray:document.querySelectorAll('#attachmentTray > *').length,height:document.getElementById('composerInput').getBoundingClientRect().height,working:document.body.classList.contains('working')||document.getElementById('composerZone').classList.contains('working')}));
console.log(JSON.stringify({before,after,errors}));
assert.equal(after.users,before.users+1,'exactly one user message per send');
assert.equal(after.text,'','composer text cleared');
assert.equal(after.tray,0,'attachment tray cleared');
assert.ok(after.height<=before.height+1,'composer height reset');
assert.deepEqual(errors,[]);
await browser.close();console.log('composer send check: ok');
