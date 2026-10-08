/* Per-agent review-profile presentation. Existing key/legacy avatar metadata retained. */
(function(root){
 'use strict';
 const key='phoenix-avatar-presentation-v1',colors=['butter','ivory','coral','rose','lilac','sky','sage','slate','lemon','tangerine','crimson','fuchsia','violet','azure','turquoise','emerald'],shapes=['round','triangle','diamond','cube'];
 const validColor=c=>typeof c==='string'&&colors.includes(c);
 function read(){try{const v=JSON.parse(localStorage.getItem(key)||'{}');return v.version===1&&v.agents&&typeof v.agents==='object'&&!Array.isArray(v.agents)?v:{version:1,agents:{}}}catch{return{version:1,agents:{}}}}
 const valid=id=>typeof id==='string'&&id.length>0&&id.length<=128&&!['__proto__','constructor','prototype'].includes(id);
 function get(id){if(!valid(id))return null;const e=read().agents[id];return e&&['fluffy','backend'].includes(e.style)&&validColor(e.palette)?{style:e.style,palette:e.palette,shape:shapes.includes(e.shape)?e.shape:'round'}:null}
 function set(id,style,palette,shape='round'){if(!valid(id)||!['fluffy','backend'].includes(style)||!validColor(palette)||!shapes.includes(shape))throw Error('Invalid avatar presentation preference');const v=read();v.agents[id]={style,palette:palette.toLowerCase(),shape};localStorage.setItem(key,JSON.stringify(v));const saved=get(id);if(saved?.style!==style||saved?.palette!==palette.toLowerCase()||saved?.shape!==shape)throw Error('Avatar preference could not be saved');dispatchEvent(new CustomEvent('phoenix:avatar-preference',{detail:{agentId:id}}));return saved}
 // Avatars are a Fluffy or the agent's own photo. Profiles saved with an older
 // drawn mark (or none) show a Fluffy in a colour stable for that agent.
 const base=colors.slice(0,8);
 function seeded(profile){const id=String(profile?.agent_id||profile?.icon_seed||'');if(!id||id==='phoenix'||id==='new-coworker')return'butter';let h=0x811c9dc5;for(const ch of id){h^=ch.charCodeAt(0);h=Math.imul(h,0x01000193)>>>0}h^=h>>>15;h=Math.imul(h,0x2c1b3c6d)>>>0;h^=h>>>12;return base[(h>>>0)%base.length]}
 const hasPhoto=b=>b?.mode==='custom'&&typeof b.custom_image_id==='string'&&b.custom_image_id.length>0;
 function effective(profile,backend={}){if(!profile||profile._avatarDraft)return backend;const p=get(profile?.agent_id),palette=p?.palette||seeded(profile),shape=p?.shape||'round';if(p?.style==='fluffy'||!hasPhoto(backend))return{...backend,mode:'fluffy',fluffy_palette:palette,fluffy_shape:shape};return{...backend,fluffy_palette:palette,fluffy_shape:shape}}
 // Called after the existing Configure command succeeds, never for preview input.
 function commit(id,form){return set(id,form.elements.avatar_mode.value==='fluffy'?'fluffy':'backend',form.elements.fluffy_palette.value,form.elements.fluffy_shape?.value||'round')}
 root.PhoenixAvatarPreferences=Object.freeze({key,colors,shapes,validColor,seeded,get,set,effective,commit,snapshot:read});
})(globalThis);
