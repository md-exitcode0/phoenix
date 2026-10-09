import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import vm from 'node:vm';

// Execute the actual player with inert image/canvas surfaces: no browser,
// native images, gateway, or agent tasks are started.
const states=['idle','thinking','browsing','coding','waiting','success','error'];
const eyes=['auto','sleepy','attentive','focused','curious','wink-left','wink-right','surprised'];
const images=[];
class FakeImage {
  constructor(){images.push(this);}
  set src(value){this.url=value;if(value)queueMicrotask(()=>this.onload?.());}
  get src(){return this.url;}
}
const context2d=()=>({clearRect(){},save(){},restore(){},translate(){},rotate(){},scale(){},putImageData(){},drawImage(image){assert.ok(image&&(!(image instanceof FakeImage)||image.src),'every rendered eye/body was loaded');},getImageData(){return {data:new Uint8ClampedArray(192*192*4)};},createImageData(){return {data:new Uint8ClampedArray(192*192*4)};}});
const canvas=()=>({getContext:()=>context2d(),setAttribute(){}});
const context={Image:FakeImage,URL,DOMException,Uint8ClampedArray,performance,queueMicrotask,setTimeout,clearTimeout,
  matchMedia:()=>({matches:false,addEventListener(){},removeEventListener(){}}),
  document:{baseURI:'http://fixture.invalid/',hidden:false,createElement:canvas,addEventListener(){},removeEventListener(){}}};
vm.runInNewContext(await readFile(new URL('../monocode/ui/fluffy-native-player.js',import.meta.url),'utf8'),context);
const manifest={version:4,frameSize:192,transition:{kind:'prop-spring',waveOverlay:false},expressions:eyes,faceCell:{x:40,y:40,width:32,height:32},states:{},layers:{butter:{}}};
for(const state of states){
  manifest.states[state]={count:1,columns:1,durationMs:1000,frameMs:1000,loop:true,staticIndex:0};
  manifest.layers.butter[state]={body:state+'-body',lightBody:state+'-light',faces:Object.fromEntries(eyes.map(eye=>[eye,state+'-'+eye])),lightFaces:Object.fromEntries(eyes.map(eye=>[eye,state+'-lit-'+eye]))};
}
const options={canvas:canvas(),manifest,paused:true,charm:false};
const full=await context.FluffyCompanion.create(options),fullCount=full.getState().loadedAssets;
full.setExpression('wink-left');full.destroy();
const scoped=await context.FluffyCompanion.create({...options,canvas:canvas(),onlyExpressions:['sleepy','attentive','focused','curious']});
assert.ok(scoped.getState().loadedAssets<fullCount,'ordinary avatars skip unused expression layers');
for(const state of states)scoped.setState(state);
for(const eye of ['sleepy','attentive','focused','curious','auto'])scoped.setExpression(eye);
assert.throws(()=>scoped.setExpression('wink-left'),/outside this scoped player/);
scoped.destroy();
assert.ok(images.every(image=>!image.src),'destroy releases all owned image surfaces');
console.log('PASS: scoped avatar image budget, all activity poses, supported eyes, full editor API and image cleanup.');
