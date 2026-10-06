/* Corrected native family routes. Only complete seven-state preset sets are offered. */
(function(root){
 'use strict';
 const pins=Object.freeze({round:'83e6a7d096ebaa166f59911611cb9d691b29dea296ac83b3774452819edfb289',triangle:'e58e530100dc31689118a2703232a1d13164e88d7958a00034354d2f7906c8a5',diamond:'3f871dc0e0682adeb8776971ea2a72a9e3a7d5d3c822a58745723afc0ddfc8e8',cube:'f9897ae25012b00c9e609c736911acf118f0acae4efa3a9646584d5d86490306'});
 const states=['idle','thinking','browsing','coding','waiting','success','error'],eyes=['auto','sleepy','attentive','focused','curious','wink-left','wink-right','surprised'];
 const fixed=Object.freeze(['butter','ivory','coral','rose','lilac','sky','sage','slate','lemon','tangerine','crimson','fuchsia','violet','azure','turquoise','emerald']);
 const color=c=>typeof c==='string'&&fixed.includes(c);
 let registry={version:2,families:{}},pending=null;const manifests=new Map();
 function offered(shape){const v=registry.families?.[shape];return Object.hasOwn(pins,shape)&&v?.sourceHash===pins[shape]&&v.renderVerification?.pass===true&&v.completedPalettes?.includes('butter')&&Object.hasOwn(v.files||{},'atlas.json')}
 function available(){return Object.keys(pins).filter(offered)}
 function ready(refresh=false){if(refresh){pending=null;manifests.clear()}if(!pending)pending=fetch('/native-family/index.json',{cache:'no-store'}).then(r=>{if(!r.ok)throw Error('Native family registry missing');return r.json()}).then(v=>{if(v?.version!==2||!v.families||typeof v.families!=='object')throw Error('Invalid native family registry');registry=v;return available()}).catch(e=>{pending=null;throw e});return pending}
 function validate(m,shape){
  if(m.sourceHash!==pins[shape]||m.version!==4||m.frameSize!==192||m.transition?.kind!=='prop-spring'||m.transition.waveOverlay!==false||!eyes.every(e=>m.expressions?.includes(e)))throw Error('Native family manifest mismatch');
  if(!m.completedPalettes?.includes('butter')||!m.layers?.butter)throw Error('Base native palette incomplete');
  if(Object.keys(m.layers).some(c=>!m.completedPalettes.includes(c)))throw Error('Unverified palette in manifest');
  for(const c of m.completedPalettes){if(!color(c))throw Error('Invalid native palette');const rows=m.layers[c];for(const s of states){const q=rows?.[s],meta=m.states?.[s];if(!q?.body||!Number.isInteger(meta?.count)||meta.count<1||!eyes.every(e=>q.faces?.[e]))throw Error('Incomplete native state '+s);if(['thinking','browsing','coding'].includes(s)&&(!q.lightBody||!eyes.every(e=>q.lightFaces?.[e])))throw Error('Missing native illumination');if(['idle','thinking','browsing','coding'].includes(s)&&!q.prop?.asset)throw Error('Missing native prop layer')}}
  if(m.exportComplete!== (m.completedPalettes.length===fixed.length))throw Error('Native completion flag mismatch');return m;
 }
 async function resolve(shape,selectedColor='butter'){
  if(!Object.hasOwn(pins,shape))throw Error('Unknown native shape');if(!color(selectedColor))throw Error('Invalid native preset');await ready();if(!offered(shape))throw Error('Native '+shape+' rendering acceptance is incomplete');
  const row=registry.families[shape];let m=manifests.get(shape);
  if(!m){const response=await fetch('/native-family/'+shape+'/atlas.json',{cache:'no-store'});if(!response.ok)throw Error('Verified native manifest missing');const bytes=await response.arrayBuffer();const sha=[...new Uint8Array(await crypto.subtle.digest('SHA-256',bytes))].map(x=>x.toString(16).padStart(2,'0')).join('');if(sha!==row.files['atlas.json'])throw Error('Native manifest bytes mismatch');m=validate(JSON.parse(new TextDecoder().decode(bytes)),shape);manifests.set(shape,m)}
  if(!Object.hasOwn(m.layers,selectedColor))throw Error('Selected native preset has no completed pack');return{manifest:m,baseURL:'/native-family/'+shape+'/'};
 }
 function palettes(shape='round'){return offered(shape)?fixed.filter(c=>registry.families[shape].completedPalettes.includes(c)):[]}
 function poster(shape,c,state='idle'){const chosen=palettes(shape).includes(c)?c:'butter';return '/native-family/'+shape+'/posters/'+encodeURIComponent(chosen)+'-'+state+'.webp'}
 root.PhoenixFluffyFamily=Object.freeze({pins,color,ready,available,resolve,palettes,poster,validate,fixedPresets:fixed});
})(globalThis);
