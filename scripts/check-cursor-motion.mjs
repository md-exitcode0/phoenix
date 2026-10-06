import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {runInNewContext} from 'node:vm';
import {travel,styles} from '../canvas-app/chromium-shell/cursor-motion-entry.mjs';
let checks=0;
for(const style of Object.keys(styles))for(const [from,aim,width,height] of [
  [{x:0,y:0},{x:1100,y:800},1100,800],
  [{x:200,y:100},{x:202,y:103},760,900],
  [{x:760,y:900},{x:0,y:0},760,900],
  [{x:55,y:65},{x:55,y:65},320,240],
]){
  const opts={style,width,height,seed:17},points=travel(from,aim,opts);
  assert.deepEqual(points[0],{t:0,...from});assert.deepEqual({x:points.at(-1).x,y:points.at(-1).y},aim);
  assert(points.at(-1).t<=800);assert(points.every((p,i)=>[p.t,p.x,p.y].every(Number.isFinite)&&p.x>=0&&p.y>=0&&p.x<=width&&p.y<=height&&(!i||p.t>=points[i-1].t)));
  assert.deepEqual(travel(from,aim,opts),points);
  assert.deepEqual(travel(from,aim,{...opts,reduced:true}),[{t:0,...from},{t:0,...aim}]);checks++;
}
assert.throws(()=>travel({x:NaN,y:0},{x:1,y:1}),TypeError);checks++;
const a=await readFile(new URL('../canvas-app/chromium-shell/cursor-motion.js',import.meta.url));
const b=await readFile(new URL('../review-shell/ui/cursor-motion.js',import.meta.url));assert(a.equals(b));checks++;
let now=0,next=0;const pending=new Map(),sandbox={performance:{now:()=>now},requestAnimationFrame:fn=>{pending.set(++next,fn);return next},cancelAnimationFrame:id=>pending.delete(id)};
runInNewContext(a.toString(),sandbox);const motion=sandbox.PhoenixCursorMotion,el={isConnected:true,style:{}};
const tick=time=>{now=time;const callbacks=[...pending.values()];pending.clear();callbacks.forEach(fn=>fn(time));};
motion.animate(el,{x:100,y:100});
let clicked=0;motion.animate(el,{x:800,y:500},{onComplete:()=>clicked++});tick(150);assert.equal(clicked,0);
motion.animate(el,{x:0,y:0},{onComplete:()=>clicked++});tick(1200);assert.equal(el.style.left,'0px');assert.equal(el.style.top,'0px');assert.equal(clicked,1);checks++;
motion.animate(el,{x:800,y:500},{onComplete:()=>clicked++});tick(1300);motion.cancel(el);const before=el.style.left;tick(2200);assert.equal(el.style.left,before);assert.equal(clicked,1);assert.equal(pending.size,0);checks++;
for(const folder of ['../canvas-app/chromium-shell/vendor/cua-motion/','../review-shell/ui/vendor/thinking-orbs/']){
  const dir=new URL(folder,import.meta.url),manifest=JSON.parse(await readFile(new URL('provenance.json',dir)));
  for(const [file,hash] of Object.entries(manifest.files)){assert.equal(createHash('sha256').update(await readFile(new URL(file,dir))).digest('hex'),hash);checks++;}
}
console.log(JSON.stringify({passed:checks,styles:Object.keys(styles),scope:'Pure trajectory, reduction, endpoint and vendored-source integrity; no page input or backend work.'}));
