// The actual thinking-orbs 0.3.2 geometry, hosted locally without a React runtime.
import {resolvePreset, MODE_FRAMES, paintFrame} from './vendor/thinking-orbs/engine.es.js';

const zone = document.getElementById('composerZone');
const header = document.getElementById('conversationHeader');
const root = document.documentElement;
const reduced = matchMedia('(prefers-reduced-motion:reduce)');
const host = document.createElement('div');
host.id = 'conversationThinking';
host.hidden = true;
host.setAttribute('role', 'status');
host.setAttribute('aria-label', 'Working');
host.innerHTML = '<canvas width="64" height="64" aria-hidden="true"></canvas>';
zone.prepend(host);
const canvas = host.firstElementChild, ctx = canvas.getContext('2d');
const preset = resolvePreset('composing', 64);
let frame = 0, started = 0, dead = false;
const diagnostics = {state:'composing', size:64, frames:0, running:false, destroyed:false};
globalThis.PhoenixThinkingOrb = {diagnostics:()=>({...diagnostics})};
function stop(){cancelAnimationFrame(frame);frame=0;diagnostics.running=false;}
function draw(now){
  frame=0;
  if(dead||host.hidden||document.hidden)return;
  const dpr=Math.min(2,devicePixelRatio||1),px=Math.round(64*dpr);
  if(canvas.width!==px){canvas.width=canvas.height=px;}
  ctx.setTransform(dpr,0,0,dpr,0,0);ctx.clearRect(0,0,64,64);
  const still=reduced.matches||root.dataset.motion==='minimal';
  paintFrame(ctx,MODE_FRAMES[preset.mode](64,still?0:(now-started)/1000*preset.speed,preset.opts),root.dataset.theme==='dark');
  diagnostics.frames++;
  diagnostics.running=!still;
  if(!still)frame=requestAnimationFrame(draw);
}
function sync(){
  if(dead)return;
  const working=zone.classList.contains('working');
  if(host.hidden===working){host.hidden=!working;started=performance.now();}
  stop();if(working&&!document.hidden)draw(performance.now());
}
const observer=new MutationObserver(sync);
observer.observe(zone,{attributes:true,attributeFilter:['class']});
observer.observe(root,{attributes:true,attributeFilter:['data-motion','data-theme']});
reduced.addEventListener('change',sync);document.addEventListener('visibilitychange',sync);

const mode=document.createElement('button');
mode.id='conversationDetailToggle';mode.type='button';
header.querySelector('.header-spacer').before(mode);
function syncMode(){const chat=root.dataset.conversationView==='chat';mode.textContent=chat?'Chat only':'Tool activity';mode.setAttribute('aria-pressed',String(!chat));mode.title=chat?'Show tool calls':'Show messages without tool calls';mode.setAttribute('aria-label',chat?'Chat only. Show tool activity':'Tool activity. Switch to chat only');}
mode.addEventListener('click',()=>{const value=root.dataset.conversationView==='chat'?'compact':'chat';globalThis.PhoenixSettings.setConversationView(value);});
addEventListener('phoenix:visual-prefs-changed',syncMode);syncMode();sync();
addEventListener('pagehide',()=>{dead=true;stop();observer.disconnect();reduced.removeEventListener('change',sync);document.removeEventListener('visibilitychange',sync);removeEventListener('phoenix:visual-prefs-changed',syncMode);diagnostics.destroyed=true;},{once:true});
