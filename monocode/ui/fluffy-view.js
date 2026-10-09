/* Frozen native player consumers. One selected-agent source canvas, mirrored after its own draw. */
(function(root){
 'use strict';
 const palettes=['butter','ivory','coral','rose','lilac','sky','sage','slate'],states=['idle','browsing','coding','thinking','waiting','success','error'];
 const title=s=>s[0].toUpperCase()+s.slice(1),escape=s=>String(s).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
 const shapes=['round','triangle','diamond','cube'],shapeLabels={round:'Round',triangle:'Triangle',diamond:'Rhombus',cube:'Marshmallow'};
 const poster=(color,state='idle',shape='round')=>PhoenixFluffyFamily.poster(shape,color,state);
 // Every shape resolves through the verified corrected family registry.
 function paletteMarkup(color,shape){return PhoenixFluffyFamily.palettes(shape).filter(c=>!c.startsWith('#')).map(c=>`<button type="button" data-fluffy-color-choice="${escape(c)}" aria-pressed="${c===color}"><img src="${poster(c,'idle',shape)}" alt="" loading="eager"><span>${title(c)}</span></button>`).join('')}
 const activity=PhoenixFluffyActivity.createStore();
 let active=null,selected='',generation=0,selectionRequest=0,dead=false,observer;const editors=new Map(),background=new Map(),backgroundFailures=new Map(),visibleTokens=new Set(),trackedTokens=new Set();let tokenObserver;
 const diagnostics={createdPlayers:0,destroyedPlayers:0,livePlayers:0,mirrors:0,frames:0,lastError:null};
 function markup(profile){const color=PhoenixAvatarPreferences.validColor(profile.fluffy_palette)?profile.fluffy_palette:'butter',shape=shapes.includes(profile.fluffy_shape)?profile.fluffy_shape:'round',id=profile.agent_id||'new-coworker',mode=activity.snapshot(id).mode;return `<span class="fluffy-token" data-fluffy-state="${mode}" data-fluffy-agent="${escape(id)}" data-fluffy-color="${escape(color)}" data-fluffy-shape="${shape}"><img src="${poster(color,mode,shape)}" alt="${shapeLabels[shape]} Fluffy"><canvas width="192" height="192" hidden aria-hidden="true"></canvas></span>`}
 function editorMarkup(color,shape='round'){return `<input type="hidden" name="fluffy_palette" value="${escape(PhoenixAvatarPreferences.validColor(color)?color:'butter')}"><input type="hidden" name="fluffy_shape" value="${shapes.includes(shape)?shape:'round'}"><div class="avatar-fluffy-controls"><fieldset class="fluffy-shapes"><legend>Shape</legend><div class="fluffy-shape-grid">${shapes.map(s=>`<button type="button" data-fluffy-shape-choice="${s}" aria-pressed="${s===shape}"><img src="${poster('butter','idle',s)}" alt="" loading="eager"><span>${shapeLabels[s]}</span></button>`).join('')}</div></fieldset><fieldset class="fluffy-palette"><legend>Color</legend><div class="fluffy-palette-grid">${paletteMarkup(color,shape)}</div><p class="fluffy-palette-status" role="status">Showing finished native colors. Only complete native presets are available.</p></fieldset><fieldset class="fluffy-previews"><legend>Preview their activity</legend><div class="fluffy-preview-grid">${states.map(s=>`<button type="button" data-fluffy-play="${s}" aria-label="${['success','error'].includes(s)?'Replay':'Play'} ${s} animation" aria-pressed="false"><span class="fluffy-preview-picture"><img src="${poster(color,s,shape)}" alt=""></span><span class="fluffy-preview-caption"><span>${title(s)}</span><svg viewBox="0 0 16 16" aria-hidden="true"><path d="m6 4 5 4-5 4Z"/></svg></span></button>`).join('')}</div></fieldset><p class="fluffy-preview-help">Choose an activity to preview it.</p><span class="fluffy-editor-error" role="status" hidden></span></div>`}
 // The native a692 player has no onFrame callback. Its own clearRect starts
 // each synchronous draw; this local context hook copies only after that draw.
 // No RAF, second simulation, polling timer or independent decode is added.
 function mirrorAfterDraw(canvas,copy){const ctx=canvas.getContext('2d'),original=ctx.clearRect;let queued=false,closed=false;ctx.clearRect=function(...args){original.apply(this,args);if(!queued&&!closed){queued=true;queueMicrotask(()=>{queued=false;if(!closed)copy()})}};return()=>{closed=true;ctx.clearRect=original}}
 async function nativePlayer(canvas,options,copy,ownerSignal){let api=null,colors=[];const unhook=mirrorAfterDraw(canvas,()=>{diagnostics.frames++;copy?.(api)}),controller=new AbortController();diagnostics.livePlayers++;diagnostics.createdPlayers++;
  const abort=()=>controller.abort();ownerSignal?.addEventListener('abort',abort,{once:true});if(ownerSignal?.aborted)abort();
  try{const route=await PhoenixFluffyFamily.resolve(options.shape||'round',options.color);colors=Object.keys(route.manifest.layers);if(controller.signal.aborted)throw new DOMException('Aborted','AbortError');api=await FluffyCompanion.create({canvas,...route,signal:controller.signal,onlyExpressions:['sleepy','attentive','focused','curious'],...options});copy?.(api)}catch(error){unhook();controller.abort();diagnostics.livePlayers--;diagnostics.destroyedPlayers++;ownerSignal?.removeEventListener('abort',abort);throw error}
  let closed=false;return{api,colors,destroy(){if(closed)return;closed=true;unhook();controller.abort();ownerSignal?.removeEventListener('abort',abort);api.destroy();diagnostics.livePlayers--;diagnostics.destroyedPlayers++}};
 }
 function mirrorToken(token,canvas,state,color){const image=token.querySelector('img'),mirror=token.querySelector('canvas');if(!mirror)return;const ctx=mirror.getContext('2d');ctx.clearRect(0,0,192,192);ctx.drawImage(canvas,0,0);mirror.hidden=false;if(image)image.hidden=true;token.dataset.fluffyColor=color;token.dataset.fluffyState=state}
 function mirrorSelected(api){if(!active)return;const snapshot=activity.snapshot(selected),native=['success','error'].includes(snapshot.mode)?api?.getState():null;
  for(const token of visibleTokens)if(token.isConnected&&token.dataset.fluffyAgent===selected)mirrorToken(token,active.canvas,snapshot.mode,active.color);diagnostics.mirrors++;
  const hero=document.querySelector('#fluffyHero');if(hero){hero.dataset.state=snapshot.mode;hero.setAttribute('aria-label',title(active.color)+' Fluffy, '+snapshot.mode)}
  if(native?.finiteComplete&&['success','error'].includes(snapshot.mode))activity.settleFinite(selected,snapshot.revision);
 }
 function detach(){generation++;if(active){const old=active;active=null;old.controller?.abort();old.player?.destroy();if(old.canvas)old.canvas.remove()}}
 async function updateActiveColor(owner,color){
  if(owner.colorTask?.color===color)return owner.colorTask.promise;if(owner.color===color&&!owner.colorTask)return;
  const task={color,promise:null};task.promise=owner.player.api.setColor(color).then(()=>{if(dead||active!==owner)return;owner.color=owner.player.api.getState().color;const hero=document.querySelector('#fluffyHero');if(hero){hero.querySelector('img').src=poster(owner.color,'idle',owner.shape);hero.querySelector('canvas').setAttribute('aria-label',title(owner.color)+' Fluffy')}mirrorSelected(owner.player.api)}).catch(error=>{if(active===owner){diagnostics.lastError=String(error);PhoenixUI.toast('Fluffy color could not load. Your saved preference is unchanged.',true)}}).finally(()=>{if(owner.colorTask===task)owner.colorTask=null});owner.colorTask=task;return task.promise;
 }
 function selectionProfile(){const item=PhoenixUI.state.selected,p=item?.kind==='agent'?PhoenixUI.profileFor(item):null;let avatar={};try{avatar=JSON.parse(p?.metadata_json||'{}').avatar||{}}catch{};return{id:p?.agent_id||'',avatar:PhoenixAvatarPreferences.effective(p,avatar)}}
 function clearSelectedFluffy(id){detach();selected=id;document.querySelector('#fluffyHero')?.remove();document.querySelector('#conversationStage')?.classList.remove('has-fluffy')}
 async function syncSelection(){if(dead||!root.PhoenixUI?.state.view)return;const request=++selectionRequest;let choice=selectionProfile();
  // Ordinary avatar identity and teardown never depend on the native registry.
  if(choice.avatar.mode!=='fluffy'){clearSelectedFluffy(choice.id);return}
  if(active&&active.id!==choice.id)clearSelectedFluffy(choice.id);selected=choice.id;
  try{await PhoenixFluffyFamily.ready()}catch(error){if(dead||request!==selectionRequest)return;choice=selectionProfile();clearSelectedFluffy(choice.id);const message=String(error),changed=diagnostics.familyRegistryError!==message;diagnostics.familyRegistryError=message;diagnostics.lastError=message;if(choice.avatar.mode==='fluffy'&&changed)PhoenixUI.toast('Fluffy animation could not load. Your saved avatar is unchanged.',true);return}
  if(dead||request!==selectionRequest||!root.PhoenixUI?.state.view)return;choice=selectionProfile();
  if(choice.avatar.mode!=='fluffy'){clearSelectedFluffy(choice.id);return}
  diagnostics.familyRegistryError=null;const {id,avatar}=choice,shape=shapes.includes(avatar.fluffy_shape)?avatar.fluffy_shape:'round',color=PhoenixFluffyFamily.palettes(shape).includes(avatar.fluffy_palette)?avatar.fluffy_palette:'butter';selected=id;
  if(active?.id===id&&active.shape===shape&&active.canvas.isConnected&&(!active.player||active.player.colors.includes(color))){active.wantedColor=color;if(active.player)await updateActiveColor(active,color);mirrorSelected(active?.player?.api);return}
  detach();const current=++generation,stage=document.querySelector('#conversationStage');stage.classList.add('has-fluffy');let hero=document.querySelector('#fluffyHero');if(!hero){hero=document.createElement('div');hero.id='fluffyHero';hero.className='fluffy-hero';stage.insertBefore(hero,document.querySelector('#conversationBody'))}
  hero.innerHTML=`<img src="${poster(color,'idle',shape)}" alt="${shapeLabels[shape]} Fluffy" class="fluffy-hero-poster"><canvas width="192" height="192" role="img" aria-label="${shapeLabels[shape]} Fluffy"></canvas>`;
  const canvas=hero.querySelector('canvas');canvas.hidden=true;active={id,color,shape,wantedColor:color,canvas,player:null,colorTask:null,controller:new AbortController()};const ownerSignal=active.controller.signal;
  try{const player=await nativePlayer(canvas,{color,shape,charm:shape==='round',state:activity.snapshot(id).mode},api=>{if(current===generation&&active?.id===id)mirrorSelected(api)},ownerSignal);
   if(dead||current!==generation){player.destroy();return}active.player=player;if(active.wantedColor!==active.color)await updateActiveColor(active,active.wantedColor);if(dead||current!==generation)return;player.api.setState(activity.snapshot(id).mode);canvas.hidden=false;hero.querySelector('img').hidden=true;mirrorSelected(player.api);
  }catch(error){if(current!==generation)return;diagnostics.lastError=String(error);hero.setAttribute('aria-label','Fluffy animation unavailable');PhoenixUI.toast('Fluffy animation could not load. Your saved avatar is unchanged.',true)}
 }
 function stopBackground(id){const owner=background.get(id);if(!owner)return;background.delete(id);owner.controller.abort();owner.player?.destroy();}
 function backgroundTokens(id){return [...visibleTokens].filter(token=>token.isConnected&&token.dataset.fluffyAgent===id&&token.closest('.company-row'))}
 function mirrorBackground(owner,api){if(dead||background.get(owner.id)!==owner)return;for(const token of backgroundTokens(owner.id))mirrorToken(token,owner.canvas,owner.mode,owner.color);if(['success','error'].includes(owner.mode)&&api?.getState().finiteComplete)activity.settleFinite(owner.id,owner.revision);}
 function syncSidebarSnapshot(snapshot){
  if(dead)return;const id=snapshot.agentId;
  if(active?.id===id){stopBackground(id);return}
  const tokens=backgroundTokens(id),busy=snapshot.mode!=='idle';
  const first=tokens[0],color=first?.dataset.fluffyColor||'butter',shape=first?.dataset.fluffyShape||'round';
  let owner=background.get(id);
  if(owner&&(owner.mode!==snapshot.mode||owner.color!==color||owner.shape!==shape||!tokens.length||!busy)){stopBackground(id);owner=null}
  if(owner){owner.revision=snapshot.revision;mirrorBackground(owner,owner.player?.api);return}
  for(const token of document.querySelectorAll('.fluffy-token[data-fluffy-agent="'+CSS.escape(id)+'"]')){
   const image=token.querySelector('img'),canvas=token.querySelector('canvas');
   if(token.dataset.fluffyState!==snapshot.mode&&image)image.src=poster(token.dataset.fluffyColor||'butter',snapshot.mode,token.dataset.fluffyShape||'round');
   token.dataset.fluffyState=snapshot.mode;if(image)image.hidden=false;if(canvas)canvas.hidden=true;
  }
  if(!busy||!tokens.length)return;
  // A failed native load must not be retried on every DOM mutation: each attempt
  // decodes a full atlas (~100 MiB), and a retry storm during a busy turn can
  // exhaust renderer memory (white-screen OOM, #278). Back off for 30 s.
  const failedAt=backgroundFailures.get(id);if(failedAt&&Date.now()-failedAt<30000)return;
  const canvas=document.createElement('canvas');canvas.width=canvas.height=192;
  owner={id,mode:snapshot.mode,color,shape,revision:snapshot.revision,canvas,player:null,controller:new AbortController()};background.set(id,owner);
  void nativePlayer(canvas,{color,shape,state:snapshot.mode,onlyState:snapshot.mode,maxVariants:24,charm:false},api=>mirrorBackground(owner,api),owner.controller.signal).then(player=>{
   if(dead||background.get(id)!==owner){player.destroy();return}owner.player=player;mirrorBackground(owner,player.api);
  backgroundFailures.delete(id);}).catch(error=>{if(background.get(id)===owner){background.delete(id);backgroundFailures.set(id,Date.now());diagnostics.lastError=String(error)}});
 }
 activity.subscribe(snapshot=>{syncSidebarSnapshot(snapshot);if(active?.id===snapshot.agentId){active.player?.api.setState(snapshot.mode);mirrorSelected(active.player?.api)}});
 function refreshEditor(form){const studio=form.querySelector('.avatar-studio'),color=form.elements.fluffy_palette?.value||'butter',shape=form.elements.fluffy_shape?.value||'round';if(!studio)return;
  let editor=editors.get(studio);if(!editor){editor={studio,form,color:'',state:'idle',request:0,player:null,controller:null,initializing:false,canvas:document.createElement('canvas'),mirror:document.createElement('canvas')};editor.canvas.width=editor.canvas.height=editor.mirror.width=editor.mirror.height=192;editor.canvas.className='fluffy-live-preview';editor.mirror.className='fluffy-studio-mirror';editors.set(studio,editor);studio.addEventListener('click',event=>{const b=event.target.closest('[data-fluffy-play]');if(b)playEditor(editor,b.dataset.fluffyPlay,true)})}
  const grid=studio.querySelector('.fluffy-palette-grid'),readyColors=PhoenixFluffyFamily.palettes(shape).filter(c=>!c.startsWith('#')),paletteKey=shape+'|'+readyColors.join(',');if(grid.dataset.nativePaletteKey!==paletteKey){grid.innerHTML=paletteMarkup(color,shape);grid.dataset.nativePaletteKey=paletteKey}
  studio.querySelector('.fluffy-palette-status').textContent='Showing '+readyColors.length+' finished native '+(readyColors.length===1?'color.':'colors.')+' Only complete native presets are available.';
  studio.querySelectorAll('[data-fluffy-shape-choice]').forEach(button=>{const s=button.dataset.fluffyShapeChoice,selected=s===shape;button.disabled=!PhoenixFluffyFamily.available().includes(s);button.classList.toggle('selected',selected);button.setAttribute('aria-pressed',String(selected));const image=button.querySelector('img');image.hidden=button.disabled;if(!button.disabled)image.src=poster(PhoenixFluffyFamily.palettes(s).includes(color)?color:'butter','idle',s)});
  studio.querySelectorAll('[data-fluffy-color-choice]').forEach(button=>{const c=button.dataset.fluffyColorChoice,selected=c===color;button.disabled=!PhoenixFluffyFamily.palettes(shape).includes(c);button.classList.toggle('selected',selected);button.setAttribute('aria-pressed',String(selected));const image=button.querySelector('img');image.hidden=button.disabled;if(!button.disabled)image.src=poster(c,'idle',shape)});
  studio.querySelectorAll('[data-fluffy-play] img').forEach(image=>image.src=poster(color,image.closest('[data-fluffy-play]').dataset.fluffyPlay,shape));
  if(studio.dataset.avatarMode!=='fluffy'){disposeEditorPlayer(editor);return}
  if(editor.shape!==shape){disposeEditorPlayer(editor);editor.shape=shape}
  const top=studio.querySelector('[data-avatar-preview]');if(editor.mirror.parentElement!==top){top.replaceChildren(editor.mirror)}
  if(editor.color!==color||!editor.player){editor.color=color;playEditor(editor,editor.state)}
 }
 function disposeEditorPlayer(e){e.request++;e.controller?.abort();e.controller=null;e.initializing=false;e.player?.destroy();e.player=null;e.canvas.remove();e.mirror.remove();e.studio.querySelectorAll('[data-fluffy-play]').forEach(b=>b.setAttribute('aria-pressed','false'))}
 async function playEditor(e,state,replay=false){if(dead||!e.studio.isConnected||e.studio.dataset.avatarMode!=='fluffy')return;if(e.initializing){e.controller?.abort();e.initializing=false;}
  if(replay&&['success','error'].includes(state)&&e.player?.api.getState().state===state){e.player.destroy();e.player=null;}
  if(e.player&&!e.player.colors.includes(e.color)){e.player.destroy();e.player=null;}
  const request=++e.request;e.state=state;
  e.studio.querySelectorAll('[data-fluffy-play]').forEach(button=>{button.setAttribute('aria-pressed',String(button.dataset.fluffyPlay===state));button.classList.toggle('playing',button.dataset.fluffyPlay===state)});
  const picture=e.studio.querySelector(`[data-fluffy-play="${state}"] .fluffy-preview-picture`);picture.append(e.canvas);
  const copy=()=>{if(e.canvas.isConnected&&e.mirror.isConnected){const ctx=e.mirror.getContext('2d');ctx.clearRect(0,0,192,192);ctx.drawImage(e.canvas,0,0)}};
  try{
   if(!e.player){e.controller=new AbortController();e.initializing=true;const player=await nativePlayer(e.canvas,{color:e.color,shape:e.shape||'round',charm:e.shape==='round',state},copy,e.controller.signal);if(request!==e.request||!e.studio.isConnected){player.destroy();return}e.player=player;e.initializing=false}
   else {if(e.player.api.getState().color!==e.color)await e.player.api.setColor(e.color);if(request!==e.request)return;e.player.api.setState(state)}
   copy();e.studio.querySelector('.fluffy-editor-error').hidden=true;
  }catch(error){if(request!==e.request||!e.studio.isConnected)return;e.initializing=false;const note=e.studio.querySelector('.fluffy-editor-error');note.hidden=false;note.textContent='Animation could not load. Choose another color or reopen Configure.';diagnostics.lastError=String(error)}
 }
 function reconcileRegistry(){if(dead||!root.PhoenixUI?.state.view)return;const view=PhoenixUI.state.view;for(const row of view.activities||[]){if(row.item?.kind==='agent'){const current=activity.snapshot(row.item.id),idle=row.status==='idle';if(idle&&current.sessionId&&current.sessionId!==row.canonical_session_id&&(view.activities||[]).some(other=>other.canonical_session_id===current.sessionId&&other.status!=='idle'&&other.active_agent_ids?.includes(row.item.id)))continue;root.PhoenixFluffies.activity.registry({agentId:row.item.id,sessionId:idle&&current.sessionId?current.sessionId:row.canonical_session_id,status:row.status,label:row.activity_label,authoritative:true});}else if(row.item?.kind==='group'&&row.status==='idle'){for(const member of view.directory.members||[]){if(member.group_id!==row.item.id)continue;const current=activity.snapshot(member.agent_id);if(current.sessionId===row.canonical_session_id)root.PhoenixFluffies.activity.registry({agentId:member.agent_id,sessionId:row.canonical_session_id,status:'idle'});}}}for(const id of new Set([...document.querySelectorAll('.company-row .fluffy-token[data-fluffy-agent]')].map(token=>token.dataset.fluffyAgent)))syncSidebarSnapshot(activity.snapshot(id));mirrorSelected(active?.player?.api);}
 function eventAgentId(event,context){
  const profiles=root.PhoenixUI?.state.view?.directory?.agents||[],owner=context.owner||{};
  const explicit=String(event.agent_id||'').trim(),supplied=String(explicit||event.agent||(owner.kind==='agent'?owner.id:'')).trim();
  if(!supplied)return '';
  const canonical=PhoenixFluffyActivity.canonical(supplied),exact=profiles.find(p=>p.agent_id===canonical);
  if(exact)return exact.agent_id;
  // Explicit IDs never inherit a nearby name or selected conversation.
  if(explicit)return '';
  const labelled=supplied.match(/^.+ \(([^()]+)\)$/),labelledId=labelled&&PhoenixFluffyActivity.canonical(labelled[1]);
  if(labelledId&&profiles.some(p=>p.agent_id===labelledId))return labelledId;
  const matches=profiles.filter(p=>[p.display_name,p.internal_role].some(value=>String(value||'').toLowerCase()===supplied.toLowerCase()));
  return matches.length===1?matches[0].agent_id:'';
 }
 function wireInput(event,context={}){if(dead||!event)return false;const scope=event.execution,id=eventAgentId(event,context);
  if(!id)return false;
  if(!scope){
   if(context.replay||context.painting||event.historical)return false;
   const row=root.PhoenixUI?.activityFor({kind:'agent',id});
   if(!row||row.canonical_session_id!==context.sessionId)return false;
   return root.PhoenixFluffies.activity.registry({agentId:id,sessionId:context.sessionId,status:row.status,label:row.activity_label,kind:event.kind,event});
  }
  const input={agentId:id,sessionId:context.sessionId||'',execution:scope,sequence:event.event_sequence,replay:context.replay||context.painting,historical:event.historical,kind:event.kind,event};
  if(event.kind==='commentary'||event.kind==='thinking'||event.kind==='reasoning'||event.kind==='narration'||event.kind==='notice')return false;
  return activity.ingest(input);
 }
 function visibleCommentary(event,node,context={}){if(!node||!event?.execution||context.replay||context.painting||event.historical)return false;
  // Native compact chat projects this authored node into .work-progress.
  // Use that exact event-bound projection when the transcript copy is folded.
  const cluster=node.closest('.work-cluster'),projection=cluster?.querySelector(':scope > .work-progress')||cluster?.closest('.team-work-block')?.querySelector(':scope > .work-progress');
  const candidate=node.getBoundingClientRect().height>0?node:projection&&cluster&&projection.dataset.progress===cluster.dataset.latestProgress?projection:node;
  const feed=document.querySelector('#conversationFeed');if(!feed||!candidate)return false;
  const r=candidate.getBoundingClientRect(),f=feed.getBoundingClientRect(),s=getComputedStyle(candidate),visible=r.height>0&&r.width>0&&s.visibility!=='hidden'&&s.display!=='none'&&r.bottom>f.top&&r.top<f.bottom;
  return activity.ingest({agentId:eventAgentId(event,context),sessionId:context.sessionId,execution:event.execution,sequence:event.event_sequence,kind:'visible_commentary',event,visible,midProgress:context.midProgress,replay:context.replay||context.painting});
 }
 function terminal(kind,context={},event={}){
  if(dead||context.replay||context.painting||context.historical||event.historical||!['turn_completed','error','stopped','canceled'].includes(kind))return false;
  const id=PhoenixFluffyActivity.canonical(context.agentId),turn=typeof context.turnId==='string'?context.turnId:'',owner=context.owner;
  const profiles=root.PhoenixUI?.state.view?.directory?.agents||[],row=root.PhoenixUI?.activityFor({kind:'agent',id});
  if(!id||!turn||!profiles.some(p=>p.agent_id===id)||!row||row.item?.kind!=='agent'||row.item.id!==id||row.canonical_session_id!==context.sessionId)return false;
  if(owner&&(owner.kind!=='agent'||PhoenixFluffyActivity.canonical(owner.id)!==id))return false;
  if(event.turn_id&&event.turn_id!==turn||(event.agent_id||event.agent)&&eventAgentId(event,context)!==id)return false;
  if(kind==='turn_completed'&&event.completion!=null&&event.completion!=='completed')return false;
  const current=activity.snapshot(id),scope=context.execution;
  if(current.sessionId!==context.sessionId||current.terminal)return false;
  if(scope){
   const key=PhoenixFluffyActivity.scopeKey(scope);
   if(scope.turn_id!==turn||!key||PhoenixFluffyActivity.scopeKey(current.execution)!==key||event.execution&&PhoenixFluffyActivity.scopeKey(event.execution)!==key)return false;
  }else if(current.execution||current.registryTurnId!==turn||event.execution)return false;
  // Completion never answers a pending ask or converts a queued turn to success.
  if(kind==='turn_completed'&&(current.waitingReason||['waiting_user','waiting_peer','queued','waiting','provider_wait'].includes(row.status)))return root.PhoenixFluffies.activity.registry({agentId:id,sessionId:context.sessionId,status:row.status,label:row.activity_label});
  if(scope)return activity.ingest({agentId:id,sessionId:context.sessionId,execution:scope,kind,event});
  // The foreground request supplies only its actual bound turn ID. No task or
  // attempt identity is manufactured for the supported legacy transport.
  return root.PhoenixFluffies.activity.registry({agentId:id,sessionId:context.sessionId,status:row.status,label:row.activity_label,kind,event:{...event,turn_id:turn}});
 }
 // Tokens drawn before the family registry loaded fell back to the butter
 // poster; once it is ready, show each token in its own colour.
 function repaintPosters(){for(const token of document.querySelectorAll('.fluffy-token')){const image=token.querySelector('img');if(image)image.src=poster(token.dataset.fluffyColor||'butter',token.dataset.fluffyState||'idle',token.dataset.fluffyShape||'round')}}
 function trackTokens(){for(const token of trackedTokens)if(!token.isConnected){tokenObserver.unobserve(token);trackedTokens.delete(token);visibleTokens.delete(token)}for(const token of document.querySelectorAll('.fluffy-token[data-fluffy-agent]'))if(!trackedTokens.has(token)){trackedTokens.add(token);tokenObserver.observe(token)}}
 function init(){tokenObserver=new IntersectionObserver(entries=>{const owners=new Set();for(const entry of entries){if(entry.isIntersecting)visibleTokens.add(entry.target);else visibleTokens.delete(entry.target);owners.add(entry.target.dataset.fluffyAgent)}for(const id of owners)syncSidebarSnapshot(activity.snapshot(id));mirrorSelected(active?.player?.api)});trackTokens();for(const name of ['phoenix:directory-updated','phoenix:directory-ready','phoenix:directory-status'])addEventListener(name,reconcileRegistry);reconcileRegistry();let bodyWorkTimer=0;const bodyWork=()=>{bodyWorkTimer=0;if(dead)return;trackTokens();for(const [studio,e] of editors)if(!studio.isConnected){disposeEditorPlayer(e);editors.delete(studio)};if(root.PhoenixUI&&active&&!active.canvas.isConnected)void syncSelection();for(const id of new Set([...document.querySelectorAll('.company-row .fluffy-token[data-fluffy-agent]')].map(token=>token.dataset.fluffyAgent)))syncSidebarSnapshot(activity.snapshot(id))};
  // Streaming chat mutates the body many times a second. Coalesce this whole-document
  // scan to one pass per 120 ms instead of one per mutation batch (#278 freezes).
  observer=new MutationObserver(()=>{if(!bodyWorkTimer)bodyWorkTimer=setTimeout(bodyWork,120)});observer.observe(document.body,{childList:true,subtree:true});addEventListener('pagehide',()=>clearTimeout(bodyWorkTimer),{once:true});
  for(const name of ['phoenix:conversation-selected','phoenix:directory-updated','phoenix:directory-ready','phoenix:avatar-preference'])addEventListener(name,syncSelection);
  PhoenixFluffyFamily.ready().then(()=>{if(dead)return;repaintPosters();for(const e of editors.values())refreshEditor(e.form)}).catch(error=>{if(!dead)diagnostics.familyRegistryError=String(error)});
 }
 function destroy(){if(dead)return;dead=true;for(const name of ['phoenix:directory-updated','phoenix:directory-ready','phoenix:directory-status'])removeEventListener(name,reconcileRegistry);observer?.disconnect();tokenObserver?.disconnect();trackedTokens.clear();visibleTokens.clear();detach();for(const e of editors.values())disposeEditorPlayer(e);editors.clear();for(const name of ['phoenix:conversation-selected','phoenix:directory-updated','phoenix:directory-ready','phoenix:avatar-preference'])removeEventListener(name,syncSelection);for(const id of [...background.keys()])stopBackground(id);activity.destroy();document.querySelector('#fluffyHero')?.remove();document.querySelector('#conversationStage')?.classList.remove('has-fluffy')}
 root.PhoenixFluffies=Object.freeze({palettes,states,markup,editorMarkup,refreshEditor,activity,wireInput,visibleCommentary,terminal,syncSelection,destroy,diagnostics(){return{...diagnostics,activeAgent:active?.id||null,activeColor:active?.color||null,activeShape:active?.shape||null,native:active?.player?.api.getState()||null,activity:activity.diagnostics(),editors:editors.size,backgroundPlayers:[...background.values()].map(e=>({id:e.id,mode:e.mode,initializing:!e.player,native:e.player?.api.getState()||null})),editorStates:[...editors.values()].map(e=>({state:e.state,color:e.color,initializing:e.initializing,native:e.player?.api.getState()||null})),destroyed:dead}}});
 addEventListener('pagehide',destroy,{once:true});if(document.readyState==='loading')addEventListener('DOMContentLoaded',init,{once:true});else init();
})(globalThis);
