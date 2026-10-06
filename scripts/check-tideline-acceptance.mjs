// Independent, model-free acceptance of the rendered Tideline concept site.
// Owns only this script and artifacts/tideline-independent-2026-09-17/**.
// Example: node scripts/check-tideline-acceptance.mjs --only booking,keyboard
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {writeFileSync} from 'node:fs';
import {mkdir, mkdtemp, readFile, writeFile, rm, stat} from 'node:fs/promises';
import {dirname, join, relative, resolve, sep} from 'node:path';
import {tmpdir} from 'node:os';
import {fileURLToPath} from 'node:url';
import {acceptanceBrowser, connectCdp} from './lib/acceptance-browser.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const owned = join(root, 'artifacts/tideline-independent-2026-09-17');
const options = Object.fromEntries(process.argv.slice(2).reduce((pairs, value, index, all) => {
  if (value.startsWith('--')) pairs.push([value.slice(2), all[index + 1]]);
  return pairs;
}, []));
const stamp = new Date().toISOString().replace(/[:.]/g, '-');
const output = resolve(root, options.output || relative(root, join(owned, 'run-' + stamp)));
assert.ok(output.startsWith(owned + sep), 'Output must be a NEW subdirectory of the owned evidence directory');
const source = resolve(root, options.source || 'artifacts/tideline-team-current-2026-09-17/work/index.html');
const url = new URL(options.url || 'http://127.0.0.1:18807/');
assert.ok(['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname), 'Only the local site is in scope');
const suites = new Set((options.only || 'booking,keyboard,layout,motion').split(','));
for (const suite of suites) assert.ok(['booking', 'keyboard', 'layout', 'motion'].includes(suite), 'Unknown suite: ' + suite);
const layoutWidths = (options.widths || '390,820,1440').split(',').map(Number);
assert.ok(layoutWidths.every(width => [390, 820, 1440].includes(width)), 'Layout widths must be 390, 820 or 1440');
await mkdir(owned, {recursive: true});
await mkdir(output, {mode: 0o700});
await mkdir(join(output, 'shots'));
// Chromium's Unix socket must fit sun_path, so keep the disposable home short.
const home = await mkdtemp(join(tmpdir(), 'tideline-independent-'));
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const sleep = ms => new Promise(resolveSleep => setTimeout(resolveSleep, ms));
const result = {startedAt: new Date().toISOString(), url: url.href, suites: [...suites], layoutWidths,
  modelCalls: 0, gatewayCalls: 0, checks: [], screenshots: [], loadedVersions: [],
  dateInputMethod: 'Native HTMLInputElement value setter plus input/change events; actual Chromium date sanitization. Submit, click, Tab, Enter and Escape use CDP Input events.',
  limits: ['Chromium only; viewport emulation is not physical iOS/WebKit testing.',
    'Credits are checked for presence and link targets, not an independent copyright/license audit.',
    'Network recording covers this isolated page target; unsafe request attempts are blocked and counted as failures.']};
let phase = 'startup', browser, ui, stopBrowser, observer, fatal;
const requests = [], responses = [], networkFailures = [], exceptions = [], protocolErrors = [];

function record(id, pass, evidence) {
  result.checks.push({id, pass: Boolean(pass), sourceSha256: result.loadedVersions.at(-1)?.sha256 || result.source?.sha256, evidence});
  console.log(JSON.stringify({check: id, pass: Boolean(pass), ...(!pass ? {evidence} : {})}));
  // Preserve completed checks even if a browser or cleanup operation fails later.
  writeFileSync(join(output, 'result.json'), JSON.stringify(result, null, 2));
}
async function checkpoint() {
  await writeFile(join(output, 'result.json'), JSON.stringify(result, null, 2));
}
async function waitFor(expression, timeout = 10000) {
  const deadline = Date.now() + timeout;
  do {
    const value = await browser.evaluate(expression);
    if (value) return value;
    await sleep(75);
  } while (Date.now() < deadline);
  throw Error('Browser condition timed out: ' + expression.slice(0, 180));
}

// A second CDP session observes events, since the shared helper exposes commands.
// Request interception protects the local-only booking contract without mocking
// the form, its handlers, the calculations, or the confirmation state.
async function observe(target) {
  const socket = new WebSocket(target), pending = new Map();
  let sequence = 0;
  await new Promise((ready, reject) => {
    socket.addEventListener('open', ready, {once: true});
    socket.addEventListener('error', () => reject(Error('Observer CDP connection failed')), {once: true});
  });
  const send = (method, params = {}) => new Promise((done, reject) => {
    const id = ++sequence;
    const timer = setTimeout(() => {pending.delete(id); reject(Error(method + ' observer timeout'));}, 10000);
    pending.set(id, {done, reject, timer});
    socket.send(JSON.stringify({id, method, params}));
  });
  socket.addEventListener('message', event => {
    const message = JSON.parse(String(event.data));
    if (message.id) {
      const call = pending.get(message.id);
      if (call) {
        pending.delete(message.id); clearTimeout(call.timer);
        message.error ? call.reject(Error(message.error.message)) : call.done(message.result);
      }
      return;
    }
    const value = message.params || {};
    if (message.method === 'Fetch.requestPaused') {
      const request = value.request, destination = new URL(request.url);
      const safeMethod = ['GET', 'HEAD'].includes(request.method);
      const safeAsset = ['fonts.googleapis.com', 'fonts.gstatic.com'].includes(destination.hostname)
        && ['Stylesheet', 'Font'].includes(value.resourceType);
      const allowed = safeMethod && (destination.origin === url.origin || safeAsset || ['data:', 'about:'].includes(destination.protocol));
      requests.push({phase, method: request.method, url: request.url, type: value.resourceType, allowed});
      send(allowed ? 'Fetch.continueRequest' : 'Fetch.failRequest', {
        requestId: value.requestId, ...(!allowed ? {errorReason: 'BlockedByClient'} : {}),
      }).catch(error => protocolErrors.push({phase, error: String(error)}));
    } else if (message.method === 'Network.responseReceived') {
      responses.push({phase, url: value.response.url, status: value.response.status, type: value.type});
    } else if (message.method === 'Network.loadingFailed') {
      networkFailures.push({phase, type: value.type, error: value.errorText, blockedReason: value.blockedReason});
    } else if (message.method === 'Runtime.exceptionThrown') {
      exceptions.push({phase, text: value.exceptionDetails?.exception?.description || value.exceptionDetails?.text});
    }
  });
  await send('Network.enable');
  await send('Runtime.enable');
  await send('Fetch.enable', {patterns: [{urlPattern: '*', requestStage: 'Request'}]});
  return {async close() {await send('Fetch.disable'); socket.close();}};
}

async function viewport(width, height) {
  await ui.evaluate(`window.__TAURI__.core.invoke('browser_surface_attach', {instance:'agent-tideline-job-independent',rect:{x:0,y:0,width:${width},height:${height}}})`);
  await browser.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: 1, mobile: false});
  await browser.send('Emulation.setFocusEmulationEnabled', {enabled: true});
}
async function navigate(label, {reduce = false, width = 1440, height = 960} = {}) {
  phase = label;
  await viewport(width, height);
  await browser.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-reduced-motion', value: reduce ? 'reduce' : 'no-preference'}]});
  const oldOrigin = await browser.evaluate('performance.timeOrigin');
  const navigation = await browser.send('Page.navigate', {url: url.href});
  if (navigation.errorText) throw Error(navigation.errorText);
  await waitFor(`performance.timeOrigin !== ${oldOrigin} && document.readyState === 'complete' && Boolean(document.querySelector('#booking-form'))`, 15000);
  const resource = await browser.send('Page.getResourceContent', {frameId: navigation.frameId, url: url.href});
  const bytes = Buffer.from(resource.content, resource.base64Encoded ? 'base64' : 'utf8');
  result.loadedVersions.push({phase, sha256: sha(bytes), time: new Date().toISOString()});
  await browser.evaluate('Promise.race([document.fonts.ready.then(()=>true),new Promise(r=>setTimeout(()=>r(false),5000))])');
  await waitFor('document.querySelector(".hero-media img").complete');
}
async function screenshot(name, fullPage = false) {
  const size = await browser.evaluate('({width:innerWidth,height:innerHeight,scrollY,documentHeight:document.documentElement.scrollHeight})');
  const params = {format: 'png', fromSurface: true, captureBeyondViewport: fullPage};
  // Keep the overview small; individual section captures retain native scale.
  const captureScale = fullPage ? Math.min(1, 6000 / size.documentHeight, 1200 / size.width) : 1;
  if (fullPage) params.clip = {x: 0, y: 0, width: size.width, height: size.documentHeight, scale: captureScale};
  assert.ok(!fullPage || size.width * size.documentHeight * captureScale ** 2 <= 8 * 1024 * 1024, 'Screenshot pixel budget');
  const capture = await browser.send('Page.captureScreenshot', params);
  const bytes = Buffer.from(capture.data, 'base64');
  assert.ok(bytes.length > 1000 && bytes.subarray(1, 4).toString() === 'PNG', 'Real nonempty PNG');
  const path = join('shots', name + '.png');
  await writeFile(join(output, path), bytes);
  result.screenshots.push({path, phase, sha256: sha(bytes), bytes: bytes.length, ...size, fullPage, captureScale});
  return sha(bytes);
}
async function scrollTo(selector) {
  await browser.evaluate(`document.querySelector(${JSON.stringify(selector)}).scrollIntoView({block:'center',behavior:'instant'})`);
  await sleep(80);
}
async function click(selector) {
  await scrollTo(selector);
  const point = await browser.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)}),r=e.getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2,visible:r.width>0&&r.height>0,hit:e===document.elementFromPoint(r.x+r.width/2,r.y+r.height/2)||e.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2))};})()`);
  assert.ok(point.visible && point.hit, 'Clickable control is visible and unobscured: ' + selector);
  await browser.send('Input.dispatchMouseEvent', {type: 'mousePressed', x: point.x, y: point.y, button: 'left', clickCount: 1});
  await browser.send('Input.dispatchMouseEvent', {type: 'mouseReleased', x: point.x, y: point.y, button: 'left', clickCount: 1});
  await sleep(70);
}
async function key(keyName, modifiers = 0) {
  const codes = {Tab: 9, Enter: 13, Escape: 27, ArrowDown: 40, ArrowUp: 38, ' ': 32};
  const code = keyName === ' ' ? 'Space' : keyName;
  const params = {key: keyName, code, windowsVirtualKeyCode: codes[keyName], nativeVirtualKeyCode: codes[keyName], modifiers};
  await browser.send('Input.dispatchKeyEvent', {...params, type: 'keyDown', ...(keyName === 'Enter' ? {text: '\r', unmodifiedText: '\r'} : {})});
  await browser.send('Input.dispatchKeyEvent', {...params, type: 'keyUp'});
  await sleep(50);
}
async function setValue(id, value) {
  return browser.evaluate(`(()=>{const e=document.getElementById(${JSON.stringify(id)});const proto=e.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLInputElement.prototype;Object.getOwnPropertyDescriptor(proto,'value').set.call(e,${JSON.stringify(value)});e.dispatchEvent(new Event('input',{bubbles:true}));e.dispatchEvent(new Event('change',{bubbles:true}));return e.value;})()`);
}
async function state() {
  return browser.evaluate(`(()=>{const form=document.querySelector('#booking-form'),confirmation=document.querySelector('#confirmation');return {values:Object.fromEntries(['room','arrival','departure','guests'].map(id=>[id,document.getElementById(id).value])),errors:Object.fromEntries(['room','arrival','departure','guests'].map(id=>[id,{message:document.getElementById(id+'-error').textContent,invalid:document.getElementById(id).getAttribute('aria-invalid'),describedBy:document.getElementById(id).getAttribute('aria-describedby')}])),formVisible:!form.hidden&&getComputedStyle(form).display!=='none',confirmed:getComputedStyle(confirmation).display!=='none',confirmation:confirmation.innerText,focus:document.activeElement.id,summary:document.querySelector('#stay-summary').textContent,roomSummary:document.querySelector('#room-summary').textContent,total:document.querySelector('#total-summary').textContent,selectedCards:[...document.querySelectorAll('.room')].filter(e=>e.querySelector('button').getAttribute('aria-pressed')==='true').map(e=>e.querySelector('h3').textContent),url:location.href};})()`);
}
async function editable() {
  if ((await state()).confirmed) await click('#edit-plan');
}
async function fill({room, arrival, departure, guests}) {
  await editable();
  await setValue('room', room);
  await setValue('arrival', arrival);
  await setValue('departure', departure);
  await setValue('guests', String(guests));
}
async function submit() {await click('#booking-form button[type="submit"]'); await sleep(120); return state();}
const currencyNumber = text => Number((text.match(/\$\s*([\d,.]+)/)?.[1] || 'NaN').replace(/,/g, ''));
const dayNumber = value => Date.parse(value + 'T12:00:00Z') / 86400000;
const addDays = (value, count) => new Date(Date.parse(value + 'T12:00:00Z') + count * 86400000).toISOString().slice(0, 10);

try {
  const initialBytes = await readFile(source), sourceStat = await stat(source);
  await writeFile(join(output, 'source-start.html'), initialBytes);
  result.source = {path: source, sha256: sha(initialBytes), mtime: sourceStat.mtime.toISOString(), bytes: initialBytes.length};
  const served = await fetch(url, {signal: AbortSignal.timeout(5000)});
  assert.equal(served.status, 200, 'Local site serves HTTP 200');
  const servedBytes = Buffer.from(await served.arrayBuffer());
  result.servedSha256 = sha(servedBytes);
  record('served-source-matches-file', result.servedSha256 === result.source.sha256, {served: result.servedSha256, file: result.source.sha256});

  const env = {...process.env, PHOENIX_HOME: home, PHOENIX_BROWSER_LOGIN_SOURCE: 'none'};
  for (const name of ['DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'DBUS_STARTER_ADDRESS', 'DBUS_STARTER_BUS_TYPE', 'XDG_ACTIVATION_TOKEN', 'PHOENIX_CHROMIUM_BRIDGE_URL', 'PHOENIX_CHROMIUM_BRIDGE_TOKEN', 'PHOENIX_BROWSER_ATTACH', 'PHOENIX_CHROMIUM_SELFTEST']) delete env[name];
  for (const [name, folder] of Object.entries({XDG_DATA_HOME: 'data', XDG_CONFIG_HOME: 'config', XDG_CACHE_HOME: 'cache', TMPDIR: 'tmp'})) {
    env[name] = join(home, folder); await mkdir(env[name], {mode: 0o700});
  }
  stopBrowser = await acceptanceBrowser({home, env, output});
  const meta = JSON.parse(await readFile(join(output, 'browser.json')));
  result.browser = {engine: meta.engine, isolatedDisplay: true, pid: meta.pid, debugPort: meta.debugPort};
  const bridgeUrl = new URL('browser/open', env.PHOENIX_CHROMIUM_BRIDGE_URL);
  bridgeUrl.searchParams.set('token', env.PHOENIX_CHROMIUM_BRIDGE_TOKEN);
  const openedResponse = await fetch(bridgeUrl, {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify({instance: 'agent-tideline-job-independent'}), signal: AbortSignal.timeout(10000)});
  assert.ok(openedResponse.ok, 'Owned browser surface opened');
  const opened = await openedResponse.json();
  const targets = await fetch(`http://127.0.0.1:${meta.debugPort}/json/list`).then(response => response.json());
  const target = targets.find(row => row.id === opened.targetId);
  const shell = targets.find(row => row.type === 'page' && row.url.includes('/ui/index.html'));
  assert.ok(target && shell, 'Owned target and shell found in owned debugger');
  browser = await connectCdp(target.webSocketDebuggerUrl);
  ui = await connectCdp(shell.webSocketDebuggerUrl);
  observer = await observe(target.webSocketDebuggerUrl);
  await browser.send('Page.enable');
  await browser.send('Network.setCacheDisabled', {cacheDisabled: true});
  await browser.send('Emulation.setTimezoneOverride', {timezoneId: 'America/Vancouver'});
  await viewport(1440, 960);
  await browser.evaluate(`document.body.innerHTML='<button id="calibrate">Native keyboard calibration</button>';document.querySelector('button').addEventListener('click',e=>e.target.dataset.activated='yes');document.querySelector('button').focus();`);
  await key('Enter');
  const calibrated = await browser.evaluate("document.querySelector('#calibrate').dataset.activated === 'yes'");
  record('harness-native-enter-calibrated', calibrated, {blankDocumentNativeButtonActivated: calibrated});
  assert.ok(calibrated, 'Keyboard harness must activate a native button before testing the site');
  await navigate('initial');
  result.clock = await browser.evaluate('({iso:new Date().toISOString(),local:new Date().toString(),zone:Intl.DateTimeFormat().resolvedOptions().timeZone,language:navigator.language})');
  const today = await browser.evaluate("(()=>{const d=new Date();return [d.getFullYear(),String(d.getMonth()+1).padStart(2,'0'),String(d.getDate()).padStart(2,'0')].join('-')})()");
  // Independent expectations use rendered prices/capacities, never data-rate,
  // the page's capacity table, its nights() or its currency formatter.
  const rooms = await browser.evaluate(`Array.from(document.querySelectorAll('.room'),e=>({name:e.querySelector('h3').innerText,price:e.querySelector('.rate').innerText,features:e.querySelector('.features').innerText}))`);
  for (const room of rooms) {room.rate = currencyNumber(room.price); room.capacity = Number(room.features.match(/(\d+)\s+guests?/i)?.[1]);}
  record('visible-room-options-and-prices', rooms.length >= 3 && rooms.every(room => room.rate > 0 && room.capacity >= 1), rooms);
  result.rooms = rooms;
  result.initialDom = await browser.evaluate(`({title:document.title,headings:[...document.querySelectorAll('h1,h2,h3')].map(e=>e.textContent),controls:[...document.querySelectorAll('button,input,select')].map(e=>({tag:e.tagName,id:e.id,type:e.type,text:e.innerText,label:e.labels?.[0]?.innerText,ariaLabel:e.getAttribute('aria-label'),min:e.min,max:e.max,options:e.options?[...e.options].map(o=>({value:o.value,text:o.text,disabled:o.disabled})):undefined})),links:[...document.querySelectorAll('a')].map(e=>({text:e.textContent,href:e.getAttribute('href')}))})`);
  await checkpoint();

  if (suites.has('booking')) {
    phase = 'booking-empty';
    const empty = await submit();
    record('empty-dates-rejected-and-focused', !empty.confirmed && empty.formVisible && empty.errors.arrival.invalid === 'true' && empty.errors.departure.invalid === 'true' && empty.focus === 'arrival' && empty.total === '—', empty);
    await screenshot('booking-empty-1440');
    const base = {room: rooms[0].name, arrival: addDays(today, 7), departure: addDays(today, 10), guests: 2};
    const invalidDates = [
      ['missing-arrival', '', base.departure, 'arrival'],
      ['missing-departure', base.arrival, '', 'departure'],
      ['past-arrival', addDays(today, -1), addDays(today, 1), 'arrival'],
      ['same-day', base.arrival, base.arrival, 'departure'],
      ['reversed-dates', base.departure, base.arrival, 'departure'],
      ['impossible-date-sanitized', '2027-02-30', base.departure, 'arrival'],
    ];
    for (const [name, arrival, departure, errorField] of invalidDates) {
      phase = 'booking-' + name;
      await fill({...base, arrival, departure});
      const actual = await submit();
      record(name, actual.formVisible && !actual.confirmed && actual.errors[errorField].invalid === 'true' && actual.focus === errorField, actual);
    }
    const availableGuests = await browser.evaluate("[...document.querySelector('#guests').options].filter(e=>!e.disabled).map(e=>Number(e.value))");
    record('guest-options-enforce-positive-visible-bound', Math.min(...availableGuests) === 1 && Math.max(...availableGuests) === Math.max(...rooms.map(room => room.capacity)) && availableGuests.every(value => Number.isInteger(value) && value > 0), availableGuests);
    for (const room of rooms) {
      for (const guestCount of [...new Set([1, room.capacity])]) {
        phase = 'booking-valid-' + room.name + '-' + guestCount;
        await fill({...base, room: room.name, guests: guestCount});
        const before = await state(), expected = 3 * room.rate;
        const actual = await submit();
        record('valid-total-' + room.name + '-' + guestCount, actual.confirmed && !actual.formVisible && actual.focus === 'confirmation' && currencyNumber(before.total) === expected && currencyNumber(actual.confirmation) === expected && before.roomSummary === room.name && before.summary.includes('3 nights') && actual.confirmation.includes(room.name) && actual.confirmation.includes(guestCount + ' guest') && actual.confirmation.includes('No reservation was made'), {expectedFromVisiblePrice: expected, before, actual});
        if (room === rooms[rooms.length - 1] && guestCount === room.capacity) await screenshot('booking-valid-1440');
        await click('#edit-plan');
        const edited = await state();
        record('edit-restores-values-' + room.name + '-' + guestCount, edited.formVisible && !edited.confirmed && edited.focus === 'room' && edited.values.room === room.name && edited.values.arrival === base.arrival && edited.values.departure === base.departure && edited.values.guests === String(guestCount), edited);
      }
      const over = availableGuests.find(value => value > room.capacity);
      if (over) {
        phase = 'booking-over-capacity-' + room.name;
        await fill({...base, room: room.name, guests: over});
        const actual = await submit();
        record('capacity-' + room.name, !actual.confirmed && actual.errors.guests.invalid === 'true' && actual.focus === 'guests', actual);
        record('capacity-error-associated-' + room.name, actual.errors.guests.describedBy?.split(/\s+/).includes('guests-error'), actual.errors.guests);
      }
    }
    const specialDates = [
      ['today-one-night', today, addDays(today, 1)],
      ['cross-month', '2027-01-31', '2027-02-02'],
      ['leap-day', '2028-02-28', '2028-03-01'],
      ['dst-fall', '2026-11-01', '2026-11-02'],
      ['dst-spring', '2027-03-14', '2027-03-15'],
    ].filter(([, arrival]) => arrival >= today);
    for (const [name, arrival, departure] of specialDates) {
      phase = 'booking-' + name;
      await fill({...base, arrival, departure});
      const actual = await submit(), nights = dayNumber(departure) - dayNumber(arrival), expected = nights * rooms[0].rate;
      record(name, actual.confirmed && currencyNumber(actual.confirmation) === expected, {nights, expected, actual});
    }
    phase = 'booking-keyboard-submit-edit';
    await fill(base);
    await browser.evaluate("document.querySelector('#booking-form button[type=submit]').focus()");
    await key('Enter');
    const keyboardConfirmed = await state();
    await key('Tab'); await key('Enter');
    const keyboardEdited = await state();
    record('keyboard-submit-and-edit', keyboardConfirmed.confirmed && keyboardConfirmed.focus === 'confirmation' && keyboardEdited.formVisible && keyboardEdited.focus === 'room', {keyboardConfirmed, keyboardEdited});
    await browser.send('Emulation.setTimezoneOverride', {timezoneId: 'Asia/Tokyo'});
    await navigate('timezone-Tokyo', {reduce: true});
    const tokyoToday = await browser.evaluate("(()=>{const d=new Date();return [d.getFullYear(),String(d.getMonth()+1).padStart(2,'0'),String(d.getDate()).padStart(2,'0')].join('-')})()");
    const tokyoArrival = addDays(tokyoToday, 7);
    await setValue('arrival', tokyoArrival);
    const minima = await browser.evaluate("({arrival:document.querySelector('#arrival').min,departure:document.querySelector('#departure').min,zone:Intl.DateTimeFormat().resolvedOptions().timeZone})");
    record('date-minima-use-local-calendar', minima.arrival === tokyoToday && minima.departure === addDays(tokyoArrival, 1), {actual: minima, expected: {arrival: tokyoToday, departure: addDays(tokyoArrival, 1)}});
    await browser.send('Emulation.setTimezoneOverride', {timezoneId: 'America/Vancouver'});
    await navigate('timezone-restored', {reduce: true});
    phase = 'booking-capacity-recovery';
    const small = rooms.find(room => room.capacity < Math.max(...availableGuests));
    const larger = rooms.find(room => room.capacity > small.capacity);
    await fill({...base, room: small.name, guests: small.capacity + 1});
    await submit();
    await setValue('room', larger.name);
    const changed = await state();
    await submit();
    await click('#edit-plan');
    const recovered = await state();
    record('room-change-clears-obsolete-capacity-error', changed.errors.guests.invalid !== 'true' && !changed.errors.guests.message && recovered.errors.guests.invalid !== 'true' && !recovered.errors.guests.message, {changed, afterConfirmAndEdit: recovered});
    await screenshot('booking-stale-capacity-error-1440');
    phase = 'booking-card-after-confirmation';
    await fill(base); await submit();
    await click('.room:nth-child(3) .room-select');
    await sleep(650);
    const afterCard = await state();
    record('room-card-after-confirmation-remains-consistent', afterCard.values.room === rooms[2].name && (afterCard.formVisible || afterCard.confirmation.includes(rooms[2].name)), afterCard);
    await screenshot('booking-room-change-after-confirmation-1440');
    const beforeRefresh = await state();
    const storage = await browser.evaluate('({local:Object.keys(localStorage),session:Object.keys(sessionStorage),query:location.search})');
    const persistencePresent = /localStorage|sessionStorage|URLSearchParams|history\.(?:pushState|replaceState)/.test(initialBytes.toString());
    await navigate('booking-refresh', {reduce: true});
    const refreshed = await state();
    result.persistence = {detectedInSource: persistencePresent, beforeRefresh, storage, afterRefresh: refreshed, status: persistencePresent ? 'present; compare state' : 'not implemented or advertised; persistence is optional in this brief'};
    if (persistencePresent) record('implemented-persistence-survives-refresh', JSON.stringify(beforeRefresh.values) === JSON.stringify(refreshed.values), result.persistence);
    await checkpoint();
  }

  if (suites.has('keyboard')) {
    await navigate('desktop-navigation', {reduce: true});
    const brokenAnchors = await browser.evaluate("[...document.querySelectorAll('a[href^=\"#\"]')].filter(a=>!document.getElementById(a.hash.slice(1))).map(a=>a.outerHTML)");
    record('internal-links-have-targets', brokenAnchors.length === 0, brokenAnchors);
    for (const id of ['stay', 'ritual', 'story', 'plan']) {
      await click('.nav-links a[href="#' + id + '"]');
      await sleep(150);
      const actual = await browser.evaluate(`({hash:location.hash,top:document.getElementById('${id}').getBoundingClientRect().top})`);
      record('desktop-nav-' + id, actual.hash === '#' + id && Math.abs(actual.top) < 3, actual);
    }
    await navigate('keyboard-skip-link', {reduce: true});
    await key('Tab');
    const skip = await browser.evaluate("({isSkip:document.activeElement.classList.contains('skip-link'),top:document.activeElement.getBoundingClientRect().top,outline:getComputedStyle(document.activeElement).outlineStyle})");
    await key('Enter');
    const skipped = await browser.evaluate("({hash:location.hash,top:document.querySelector('main').getBoundingClientRect().top})");
    record('keyboard-skip-link', skip.isSkip && skip.top >= 0 && skip.outline !== 'none' && skipped.hash === '#main', {skip, skipped});
    for (const width of [390, 820]) {
      await navigate('keyboard-menu-' + width, {reduce: true, width, height: 960});
      await browser.evaluate("document.querySelector('.menu-button').focus()");
      await key('Enter');
      await waitFor("getComputedStyle(document.querySelector('.nav-links')).visibility === 'visible'", 2000).catch(() => false);
      const opened = await browser.evaluate("({expanded:document.querySelector('.menu-button').getAttribute('aria-expanded'),visible:getComputedStyle(document.querySelector('.nav-links')).visibility,overflow:document.body.style.overflow})");
      record('menu-keyboard-opens-' + width, opened.expanded === 'true' && opened.visible === 'visible', opened);
      const tabSequence = [];
      for (let index = 0; index < 6; index++) {
        await key('Tab');
        tabSequence.push(await browser.evaluate("(()=>{const e=document.activeElement,r=e.getBoundingClientRect(),hit=document.elementFromPoint(r.x+r.width/2,r.y+r.height/2);return {tag:e.tagName,text:e.textContent.trim().slice(0,80),id:e.id,inMenu:!!e.closest('.nav-links,.menu-button'),unobscured:hit===e||e.contains(hit)}})()"));
      }
      record('overlay-keyboard-focus-visible-' + width, tabSequence.every(item => item.inMenu && item.unobscured), tabSequence);
      await key('Escape');
      const escaped = await browser.evaluate("({expanded:document.querySelector('.menu-button').getAttribute('aria-expanded'),overflow:document.body.style.overflow,focusedMenu:document.activeElement.classList.contains('menu-button')})");
      record('overlay-escape-closes-' + width, escaped.expanded === 'false' && escaped.overflow !== 'hidden' && escaped.focusedMenu, escaped);
      if (escaped.expanded !== 'true') await click('.menu-button');
      await screenshot('menu-open-' + width);
      await click('.nav-links a[href="#stay"]');
      const closed = await browser.evaluate("({expanded:document.querySelector('.menu-button').getAttribute('aria-expanded'),overflow:document.body.style.overflow,hash:location.hash})");
      record('menu-link-closes-and-navigates-' + width, closed.expanded === 'false' && closed.overflow !== 'hidden' && closed.hash === '#stay', closed);
    }
    await navigate('menu-resize', {reduce: true, width: 390, height: 844});
    await click('.menu-button'); await viewport(1440, 960);
    // Device metrics resolve before Chromium dispatches the media-query change.
    // Observe the rendered state after that event, not the transient CSS-only
    // frame in which the button is hidden but its listener has not run yet.
    await browser.evaluate('new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))');
    const resized = await browser.evaluate("({buttonDisplay:getComputedStyle(document.querySelector('.menu-button')).display,overflow:getComputedStyle(document.body).overflow,expanded:document.querySelector('.menu-button').getAttribute('aria-expanded')})");
    record('desktop-resize-restores-page-scrolling', resized.buttonDisplay !== 'none' || resized.overflow !== 'hidden', resized);
    await checkpoint();
  }

  if (suites.has('layout')) {
    for (const [width, height] of [[390, 844], [820, 1180], [1440, 960]].filter(([width]) => layoutWidths.includes(width))) {
      await navigate('layout-' + width, {width, height});
      await screenshot('hero-' + width);
      const totalHeight = await browser.evaluate('document.documentElement.scrollHeight');
      for (let y = 0; y <= totalHeight; y += height * .75) {
        await browser.evaluate(`window.scrollTo({top:${Math.round(y)},behavior:'instant'})`); await sleep(75);
      }
      await waitFor("[...document.images].every(e=>e.complete)");
      await sleep(1000);
      const layout = await browser.evaluate(`({viewport:innerWidth,documentWidth:document.documentElement.scrollWidth,height:document.documentElement.scrollHeight,images:[...document.images].map(e=>({src:e.currentSrc,alt:e.alt,complete:e.complete,naturalWidth:e.naturalWidth,naturalHeight:e.naturalHeight})),textBoxes:[...document.querySelectorAll('.room-copy,.room-copy h3,.room-copy p,.rate,.footer-mark,.planner-intro h2')].map(e=>({text:e.innerText.slice(0,90),width:e.getBoundingClientRect().width,clientWidth:e.clientWidth,scrollWidth:e.scrollWidth,height:e.getBoundingClientRect().height})),credits:[...document.querySelectorAll('.credits a')].map(e=>({text:e.textContent,url:e.href})),hiddenReveals:[...document.querySelectorAll('.reveal')].filter(e=>Number(getComputedStyle(e).opacity)<.99).map(e=>e.className)})`);
      record('no-horizontal-page-overflow-' + width, layout.documentWidth <= width + 1, {viewport: width, documentWidth: layout.documentWidth});
      const overflow = layout.textBoxes.filter(box => box.scrollWidth > box.clientWidth + 2);
      record('room-and-heading-text-fits-' + width, overflow.length === 0, overflow);
      record('all-images-loaded-' + width, layout.images.length >= 6 && layout.images.every(image => image.complete && image.naturalWidth > 0), layout.images);
      record('all-scroll-reveals-visible-' + width, layout.hiddenReveals.length === 0, layout.hiddenReveals);
      record('image-credits-present-' + width, layout.credits.some(credit => /commons\.wikimedia\.org/.test(credit.url)) && layout.credits.some(credit => /creativecommons\.org/.test(credit.url)), layout.credits);
      result['layout' + width] = layout;
      await scrollTo('.room:nth-child(2)'); await sleep(150);
      await screenshot('rooms-' + width);
      if (options.overview === 'true') {
        await browser.evaluate('window.scrollTo({top:0,behavior:"instant"})');
        await screenshot('full-' + width, true);
      }
      await fill({room: rooms[2].name, arrival: addDays(today, 7), departure: addDays(today, 10), guests: rooms[2].capacity});
      const valid = await submit();
      record('responsive-booking-' + width, valid.confirmed && valid.focus === 'confirmation' && currencyNumber(valid.confirmation) === 3 * rooms[2].rate, valid);
      await screenshot('confirmation-' + width);
      await checkpoint();
    }
    const images = [...new Set(result['layout' + layoutWidths.at(-1)].images.map(image => image.src))];
    result.imageFiles = [];
    for (const image of images) {
      const imageUrl = new URL(image);
      if (imageUrl.origin !== url.origin) continue;
      const path = resolve(dirname(source), '.' + decodeURIComponent(imageUrl.pathname));
      if (!path.startsWith(dirname(source) + sep)) continue;
      const bytes = await readFile(path);
      result.imageFiles.push({path: relative(root, path), bytes: bytes.length, sha256: sha(bytes)});
    }
  }

  if (suites.has('motion')) {
    const motionState = () => browser.evaluate(`({time:performance.now(),reduce:matchMedia('(prefers-reduced-motion: reduce)').matches,hero:getComputedStyle(document.querySelector('.hero-media img')).transform,cue:getComputedStyle(document.querySelector('.scroll-cue'),'::after').transform,animations:document.getAnimations().map(a=>({name:a.animationName,time:a.currentTime,state:a.playState,duration:a.effect.getComputedTiming().duration})),scrollBehavior:getComputedStyle(document.documentElement).scrollBehavior})`);
    await navigate('motion-normal');
    const start = await motionState(); await screenshot('motion-normal-t0');
    await sleep(800);
    const end = await motionState(); await screenshot('motion-normal-t1');
    record('normal-motion-changes-over-time', !start.reduce && start.hero !== end.hero && start.cue !== end.cue && end.time > start.time + 700, {start, end});
    const hidden = await browser.evaluate("({opacity:getComputedStyle(document.querySelector('.room')).opacity,transform:getComputedStyle(document.querySelector('.room')).transform})");
    await scrollTo('.room'); await sleep(100);
    const mid = await browser.evaluate("({opacity:getComputedStyle(document.querySelector('.room')).opacity,transform:getComputedStyle(document.querySelector('.room')).transform})");
    await sleep(1000);
    const shown = await browser.evaluate("({opacity:getComputedStyle(document.querySelector('.room')).opacity,transform:getComputedStyle(document.querySelector('.room')).transform})");
    record('scroll-reveal-actually-animates', Number(hidden.opacity) < .01 && Number(mid.opacity) > 0 && Number(mid.opacity) < 1 && Number(shown.opacity) > .99, {hidden, mid, shown});
    await navigate('motion-reduced', {reduce: true}); await sleep(100);
    const reducedStart = await motionState(); const shot0 = await screenshot('motion-reduced-t0');
    await sleep(800);
    const reducedEnd = await motionState(); const shot1 = await screenshot('motion-reduced-t1');
    const invisible = await browser.evaluate("[...document.querySelectorAll('.reveal')].filter(e=>Number(getComputedStyle(e).opacity)<.99).length");
    record('reduced-motion-is-static-and-content-visible', reducedStart.reduce && reducedStart.hero === reducedEnd.hero && reducedStart.cue === reducedEnd.cue && reducedEnd.scrollBehavior === 'auto' && reducedEnd.animations.every(animation => animation.duration <= .01 || animation.state !== 'running') && invisible === 0, {start: reducedStart, end: reducedEnd, invisible, identicalScreenshotPixelsEncoded: shot0 === shot1});
    await navigate('motion-live-preference');
    await scrollTo('.room:nth-child(3)'); await sleep(900);
    await browser.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-reduced-motion', value: 'reduce'}]});
    await sleep(150);
    await click('.room:nth-child(3) .room-select');
    const liveMotion = await browser.evaluate("({reduce:matchMedia('(prefers-reduced-motion: reduce)').matches,plannerTop:document.querySelector('#plan').getBoundingClientRect().top,heroTransform:getComputedStyle(document.querySelector('.hero-media img')).transform})");
    record('changed-reduced-motion-preference-stops-room-scroll-animation', liveMotion.reduce && Math.abs(liveMotion.plannerTop) < 3, liveMotion);
  }
  phase = 'final';
  const finalBytes = await readFile(source);
  result.finalSourceSha256 = sha(finalBytes);
  record('source-version-stable-during-run', result.finalSourceSha256 === result.source.sha256 && result.loadedVersions.every(version => version.sha256 === result.source.sha256), {start: result.source.sha256, end: result.finalSourceSha256, loaded: result.loadedVersions});
} catch (error) {
  fatal = String(error.stack || error);
  result.fatal = fatal;
  console.error(fatal);
  process.exitCode = 2;
} finally {
  if (observer) {
    const submissions = requests.filter(request => request.phase.startsWith('booking-') && request.phase !== 'booking-refresh' && (['Document', 'XHR', 'Fetch', 'Ping', 'WebSocket'].includes(request.type) || !['GET', 'HEAD'].includes(request.method)));
    if (suites.has('booking')) record('booking-causes-no-submission-request', submissions.length === 0, submissions);
    record('no-external-booking-or-mutating-request-attempt', requests.every(request => request.allowed), requests.filter(request => !request.allowed));
    record('no-page-runtime-exceptions', exceptions.length === 0, exceptions);
    record('network-observer-remained-valid', protocolErrors.length === 0, protocolErrors);
  }
  const cleanupErrors = [];
  try {await observer?.close();} catch (error) {cleanupErrors.push('observer: ' + String(error));}
  browser?.close(); ui?.close();
  try {await stopBrowser?.();} catch (error) {cleanupErrors.push('browser: ' + String(error));}
  if (!cleanupErrors.length) {
    try {await rm(home, {recursive: true, force: true, maxRetries: 5, retryDelay: 100});}
    catch (error) {cleanupErrors.push('private home removal: ' + String(error));}
  }
  result.cleanup = {browserStopped: Boolean(stopBrowser) && !cleanupErrors.length, privateHomeRemoved: !cleanupErrors.length, errors: cleanupErrors};
  result.finishedAt = new Date().toISOString();
  result.counts = {passed: result.checks.filter(check => check.pass).length, failed: result.checks.filter(check => !check.pass).length};
  await writeFile(join(output, 'network.json'), JSON.stringify({requests, responses, networkFailures, exceptions, protocolErrors}, null, 2));
  await checkpoint();
  console.log(JSON.stringify({output: relative(root, output), ...result.counts, fatal: Boolean(fatal), cleanup: result.cleanup}));
  if (!fatal && (result.counts.failed || cleanupErrors.length)) process.exitCode = 1;
}
