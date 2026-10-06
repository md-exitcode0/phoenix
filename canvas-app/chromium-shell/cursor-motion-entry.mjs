// MIT Cua motion-lab algorithms, pinned in vendor/cua-motion/provenance.json.
import {candidates} from './vendor/cua-motion/candidates.js';
import {resolveParams} from './vendor/cua-motion/plan.js';
import {Rng} from './vendor/cua-motion/rng.js';
export const styles=Object.freeze({signature_arc:'dc-signature-arc',spring_settle:'dc-spring-settle',magnetic:'dc-magnetic',comet_swoop:'dc-comet-swoop',adaptive:'adaptive-auto',classic:'dubins-glide'});
export function travel(from,aim,{style='signature_arc',width=1280,height=800,reduced=false,seed=1}={}){
  if(![from.x,from.y,aim.x,aim.y,width,height].every(Number.isFinite)||width<=0||height<=0)throw new TypeError('Finite cursor coordinates required');
  if(reduced||Math.hypot(aim.x-from.x,aim.y-from.y)<0.01)return [{t:0,...from},{t:0,...aim}];
  const candidate=candidates.find(c=>c.id===styles[style])||candidates.find(c=>c.id===styles.signature_arc);
  const p=resolveParams(candidate),target={x:aim.x-12,y:aim.y-12,w:24,h:24,action:'hover'};
  const result=candidate.move({from,aim,target,rng:new Rng(String(seed)),p,index:0,state:{pressed:false},action:'hover',prev:from,next:null,scene:{start:from,waypoints:[target]},stage:{w:width,h:height}});
  const samples=Array.isArray(result)?result:result.samples;
  if(!samples.length||samples.some(q=>![q.t,q.x,q.y].every(Number.isFinite)))throw new Error('Invalid upstream cursor trajectory');
  const duration=samples.at(-1).t,scale=duration>800?800/duration:1;
  // Preserve the actual reported target. Animation never changes page input.
  const out=samples.map(q=>({t:q.t*scale,x:Math.max(0,Math.min(width,q.x)),y:Math.max(0,Math.min(height,q.y))}));
  out[0]={t:0,...from};out[out.length-1]={t:duration*scale,...aim};return out;
}
export function animate(element,aim,options={}){
  const previous=element.__phoenixMotion;
  if(previous)cancelAnimationFrame(previous.frame);
  const from=previous?.position||aim,job={position:from,frame:0};element.__phoenixMotion=job;
  const points=travel(from,aim,options),start=performance.now();let index=0;
  function draw(now){
    if(element.__phoenixMotion!==job||!element.isConnected)return;
    const t=now-start;while(index<points.length-2&&points[index+1].t<t)index++;
    const a=points[index],b=points[Math.min(index+1,points.length-1)],f=b.t>a.t?Math.max(0,Math.min(1,(t-a.t)/(b.t-a.t))):1;
    job.position={x:a.x+(b.x-a.x)*f,y:a.y+(b.y-a.y)*f};element.style.left=job.position.x+'px';element.style.top=job.position.y+'px';
    if(t<points.at(-1).t)job.frame=requestAnimationFrame(draw);else {job.frame=0;options.onComplete?.();}
  }
  draw(start);return job;
}
export function cancel(element){const job=element?.__phoenixMotion;if(job)cancelAnimationFrame(job.frame);if(element)delete element.__phoenixMotion;}
