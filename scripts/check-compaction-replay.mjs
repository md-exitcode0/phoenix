import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
const source=readFileSync(new URL('../canvas-app/ui/conversation.js',import.meta.url),'utf8');
const fn=source.slice(source.indexOf('  function renderContextCompaction('),source.indexOf('  function humanTool('));
function classes(){const set=new Set();return {add:x=>set.add(x),remove:x=>set.delete(x),contains:x=>set.has(x),toggle:(x,on)=>on?set.add(x):set.delete(x)};}
const rows=[], list={querySelectorAll:()=>rows,append:row=>rows.push(row)},cluster={classList:classes(),querySelector:()=>list};
const state={item:{kind:'group'},painting:true,renderingTurnId:'one'};
const context={state,CSS:{escape:x=>x},canonicalAgentId:x=>x,ensureWorkCluster:()=>cluster,agentLabel:x=>x,compactionIconMarkup:()=>'',pixelLoaderMarkup:()=>'',formatTokenCount:String,syncActivityCursor:()=>{},scrollLatest:()=>{},updateContext:()=>{},document:{createElement:()=>{const fields={strong:{},small:{}};return {dataset:{},classList:classes(),querySelector:x=>fields[x]};}}};
vm.createContext(context);vm.runInContext(fn+';this.render=renderContextCompaction;',context);
const send=(status,replay=true)=>context.render({agent:'Theo',status},replay);
send('started');assert.equal(rows[0].classList.contains('running'),false);
send('completed');assert.equal(rows.length,1,'replayed completion must settle the original start');assert.equal(rows[0].dataset.status,'completed');
send('started');send('failed');assert.equal(rows.length,2);assert.equal(rows[1].classList.contains('failed'),true);
send('started');state.renderingTurnId='two';send('completed');assert.equal(rows.length,4,'another turn cannot settle an old start');assert.equal(rows[2].dataset.status,'started');
state.painting=false;send('started',false);assert.equal(rows.at(-1).classList.contains('running'),true);send('completed',false);assert.equal(rows.at(-1).classList.contains('running'),false);
console.log('PASS: replay and live compaction pairing, failure visibility, turn isolation');
