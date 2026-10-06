// Deterministic async lifecycle checks against the production viewer script.
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import vm from 'node:vm';
class Element {
  hidden=false;value='';textContent='';children=[];handlers={};
  addEventListener(name,callback){this.handlers[name]=callback;}
  removeAttribute(name){delete this[name];}
  replaceChildren(){this.children=[];}
  append(child){this.children.push(child);}
}
const ids=['desktopWorkspacePicker','desktopObservationImage','desktopObservationEmpty','desktopObservationCaption','desktopWorkspaceStatus'];
const elements=Object.fromEntries(ids.map(id=>[id,new Element()]));
const timers=new Map();let sequence=0;
const document={hidden:false,getElementById:id=>elements[id],createElement:()=>new Element(),addEventListener(){}};
const window={};
vm.runInNewContext(await readFile(new URL('../canvas-app/ui/desktop-viewer.js',import.meta.url),'utf8'),
  {window,document,AbortController,Date,setTimeout:fn=>{timers.set(++sequence,fn);return sequence;},clearTimeout:id=>timers.delete(id)});
const flush=async()=>{for(let i=0;i<10;i++)await Promise.resolve();};
const poll=async()=>{const [id,fn]=timers.entries().next().value;timers.delete(id);fn();await flush();};
let label='Leo',timestamp=1000,workspaces=[{scope_key:'scope',label,running:true,in_use:false}];
const requests=[];
let delay=null;
const rpc=async request=>{
  requests.push(request);
  if(request==='DesktopWorkspaces')return {DesktopWorkspaces:workspaces.map(workspace=>workspace.scope_key==='scope'?{...workspace,label}:workspace)};
  if(delay)return await delay;
  return {DesktopObservation:{scope_key:request.DesktopObservation.scope_key,captured_at_ms:timestamp,kind:'desktop',data_url:timestamp===null?null:request.DesktopObservation.after_ms===timestamp?null:'data:image/png;base64,test'}};
};
window.PhoenixDesktopViewer.update({visible:true,context:'first',rpc});await flush();
const image=elements.desktopObservationImage;
assert.equal(image.hidden,false);
assert.match(image.alt,/Leo/);
label='Renamed coworker';await poll();
assert.match(image.alt,/Renamed coworker/,'rename must update accessible image attribution even without new pixels');
timestamp=null;await poll();
assert.equal(image.hidden,true,'recreated scope with no capture must clear previous pixels');
assert.equal(image.src,undefined);
assert.match(elements.desktopObservationEmpty.textContent,/first screen observation/);
timestamp=2000;await poll();
assert.equal(requests.at(-1).DesktopObservation.after_ms,null,'new capture must not be suppressed by an old timestamp');
assert.equal(image.hidden,false);
let finishOld;delay=new Promise(resolve=>{finishOld=resolve;});await poll();
window.PhoenixDesktopViewer.update({visible:false,context:'second',rpc});
finishOld({DesktopObservation:{scope_key:'scope',captured_at_ms:3000,data_url:'data:image/png;base64,stale'}});await flush();
assert.equal(image.hidden,true,'late response must not restore old context pixels');
assert.equal(image.src,undefined);
assert.equal(timers.size,0,'hidden context must not schedule more polling');
delay=null;timestamp=4000;workspaces=[
  {scope_key:'school-scope',label:'school_coach',running:true,in_use:true},
  {scope_key:'avery-scope',label:'frontend',running:true,in_use:false},
  {scope_key:'sales-scope',label:'sales',running:true,in_use:false},
];
window.PhoenixDesktopViewer.update({visible:true,context:'group:launch',owners:[
  {id:'frontend',label:'Avery',aliases:['frontend']},
  {id:'school_coach',label:'School Coach',aliases:['school_coach']},
],rpc});await flush();
assert.deepEqual(elements.desktopWorkspacePicker.children.map(option=>option.textContent),['Avery','School Coach'],'group picker must include only member desktops with directory names');
assert.equal(elements.desktopWorkspacePicker.value,'avery-scope','group picker follows member order instead of the global alphabetic desktop inventory');
window.PhoenixDesktopViewer.update({visible:false,context:'group:launch',owners:[],rpc});
console.log('PASS: rename attribution, recreated scope, new capture, stale response rejection, hidden polling, scoped group desktops and friendly labels');
