/* Presentation adapter on the copied Phoenix UI, derived from pinned MonoCode v0.3.0.
   Uses existing controls, never replaces controller/command implementations. */
(function(root){
 'use strict';const $=id=>document.getElementById(id),mono=document.documentElement.dataset.skin==='phoenix';
 function ready(){return Boolean(root.PhoenixUI?.state.view&&root.PhoenixConversation)}
 function init(){
  const ui=PhoenixUI,stage=$('conversationStage'),feed=$('conversationFeed'),zone=$('composerZone'),composer=$('composer');
  // Median native idle coat colors. Only the Activity orb uses this palette.
  const activityColors={butter:'#aa8444',ivory:'#9d9687',coral:'#9f5f4a',rose:'#925f72',lilac:'#746c96',sky:'#537897',sage:'#6a8262',slate:'#535d64'};
  function activityColor(){const item=ui.state.selected,p=ui.profileFor(item);let avatar={};try{avatar=JSON.parse(p?.metadata_json||'{}').avatar||{}}catch{};avatar=PhoenixAvatarPreferences.effective(p,avatar);const color=avatar.mode==='fluffy'?activityColors[avatar.fluffy_palette]||activityColors.butter:ui.profileColor(p),html=document.documentElement;if(html.style.getPropertyValue('--activity-color')===color)return;html.style.setProperty('--activity-color',color);dispatchEvent(new CustomEvent('phoenix:activity-color-changed',{detail:{color,agentId:p?.agent_id}}))}
  for(const event of ['phoenix:conversation-selected','phoenix:directory-updated','phoenix:avatar-preference'])addEventListener(event,activityColor);
  activityColor();addEventListener('pagehide',()=>{for(const event of ['phoenix:conversation-selected','phoenix:directory-updated','phoenix:avatar-preference'])removeEventListener(event,activityColor)},{once:true});
  // The review's first default is frontend-only. A saved backend choice, including
  // opting out of Fluffy, is never replaced on a subsequent launch.
  if(!localStorage.getItem('phoenix-review-avatar-seeded')){PhoenixAvatarPreferences.set('phoenix','fluffy','butter');localStorage.setItem('phoenix-review-avatar-seeded','1');ui.refreshDirectory();PhoenixFluffies.syncSelection()}
  // A warm native profile can finish directory loading before the native view's
  // ready listener mounts. Reconcile the selected preference once after startup.
  void PhoenixFluffies.syncSelection();
  document.querySelectorAll('.avatar-drop small').forEach(el=>el.textContent='PNG, JPEG, WebP, or AVIF');
  if(!mono){document.documentElement.dataset.reviewReady='true';return}
  // This correction starts opaque and compact. No company/backend preference is changed.
  document.documentElement.dataset.mcGlass=String(root.PhoenixIsolatedBackend?.transparent===true);
  ui.applyVisualPrefs(ui.visualPrefs());
  // Window controls belong to the full window, never the resizable main pane.
  const controls=document.querySelector('.chrome-controls');controls.classList.add('mc-window-controls');document.body.append(controls);
  // Keep the original mounted control/listener anchored above the composer.
  const jump=$('jumpLatest');if(jump)zone.insertBefore(jump,$('taskBlock'));
  // No extra Write next message plus. Attachment and browser plus stay mounted.
  const label=document.createElement('h1');label.className='mc-empty-heading';label.textContent='What should we work on?';zone.prepend(label);
  const head=document.createElement('div');head.className='mc-composer-head';head.innerHTML='<span class="mc-context" id="mcContextUsage"></span>';composer.prepend(head);
  $('composerInput').dataset.placeholder='Ask, build, / for commands, @ for references...';
  const toolbar=composer.querySelector('.composer-toolbar'),options=$('composerOptions');
  // Reuse the mounted controls, outside <details>' closed-content behavior.
  // Their native listeners stay attached and Escape still closes their menus.
  for(const control of [...$('composerOptionsPanel').children])options.before(control);
  options.hidden=true;
  // These are one action group when the toolbar wraps, not two flex items
  // that can leave Send stranded on its own row. Keep their real listeners.
  const actions=document.createElement('div');actions.className='mc-composer-actions';actions.append($('voiceButton'),$('sendButton'));toolbar.append(actions);
  // Preserve the original Send/queue/Stop control and its real listeners.
  // Keep the same mounted contenteditable/draft/attachments while docking.
  let empty=null,capture=null,animation=null;
  function layout(){const isEmpty=!feed.querySelector('.message-row,.work-cluster,.team-work-block,.user-bubble,.conversation-loading,.conversation-load-error');if(empty!==isEmpty){empty=isEmpty;stage.classList.toggle('mc-empty',isEmpty);label.hidden=!isEmpty;if(!isEmpty&&capture&&performance.now()-capture.at<1500&&!matchMedia('(prefers-reduced-motion:reduce)').matches&&document.documentElement.dataset.motion!=='minimal'){const to=zone.getBoundingClientRect();animation?.cancel();animation=zone.animate([{transform:`translate(${capture.x-to.x}px,${capture.y-to.y}px)`},{transform:'translate(0,0)'}],{duration:480,easing:'cubic-bezier(0.22,1,0.36,1)'});animation.finished.catch(()=>{}).then(()=>{animation=null})}capture=null}const text=$('contextPercent')?.textContent||'',limit=$('composerContext')?.textContent||'';$('mcContextUsage').textContent=(text==='—'?'':text+'% context used')}
  composer.addEventListener('submit',()=>{const r=zone.getBoundingClientRect();capture={x:r.x,y:r.y,at:performance.now()}},{capture:true});
  function projectCommentary(){
   // Authored commentary now has a single real transcript block. Retire only
   // this adapter's derived copies, never stored conversation rows.
   feed.querySelectorAll('.work-cluster > .mc-commentary').forEach(node=>node.remove());
  }
  // Local review reentry keeps unfinished Configure fields without committing
  // them. Only this non-credential form is eligible; vault/login forms stay out.
  const formDraftKey='phoenix-review-configure-draft-v1';let reentering=false;
  function rememberConfigureDraft(){const form=$('editForm');if(!form?.dataset.configureId||reentering)return;const fields=[...form.elements].filter(el=>el.name&&!['file','password'].includes(el.type)).map(el=>({name:el.name,value:el.value,checked:el.checked,type:el.type}));try{localStorage.setItem(formDraftKey,JSON.stringify({item:{kind:form.dataset.configureKind,id:form.dataset.configureId},fields,customAvatarDataUrl:form.dataset.customAvatarDataUrl||null}));}catch{}}
  $('modalLayer').addEventListener('input',rememberConfigureDraft);$('modalLayer').addEventListener('change',rememberConfigureDraft);$('modalLayer').addEventListener('phoenix:modal-closing',()=>{if($('editForm'))localStorage.removeItem(formDraftKey)});addEventListener('pagehide',rememberConfigureDraft);addEventListener('phoenix:review-reentry',rememberConfigureDraft);
  const settingsKey='phoenix-review-settings-position-v1';let settingsRestoring=false;
  function rememberSettings(){if(settingsRestoring)return;const open=document.body.classList.contains('settings-open'),section=$('settingsPagePicker')?.querySelector('span')?.textContent||'General',scope=$('settingsScope')?.dataset.value||'global';try{localStorage.setItem(settingsKey,JSON.stringify({open,section,scope,scroll:$('settingsMain')?.scrollTop||0}));}catch{}}
  addEventListener('phoenix:settings-visibility',rememberSettings);$('settingsView').addEventListener('click',()=>queueMicrotask(rememberSettings));$('settingsView').addEventListener('scroll',rememberSettings,{capture:true});addEventListener('pagehide',rememberSettings);addEventListener('phoenix:review-reentry',rememberSettings);
  async function restoreLocalForms(){let saved=null;try{saved=JSON.parse(localStorage.getItem(formDraftKey)||'null');}catch{}if(saved&&['agent','group'].includes(saved.item?.kind)&&ui.profileFor(saved.item)?.lifecycle==='active'&&$('modalLayer').hidden){reentering=true;try{ui.configureItem(saved.item);const form=$('editForm');for(const field of saved.fields||[]){const el=[...form.elements].find(e=>e.name===field.name&&e.type===field.type&&(e.type!=='radio'||e.value===field.value));if(!el)continue;if(['checkbox','radio'].includes(el.type))el.checked=field.checked;else el.value=field.value;el.dispatchEvent(new Event('change',{bubbles:true}));}if(saved.customAvatarDataUrl?.startsWith('data:image/'))form.dataset.customAvatarDataUrl=saved.customAvatarDataUrl;PhoenixFluffies.refreshEditor(form);}finally{reentering=false;}}
   let position=null;try{position=JSON.parse(localStorage.getItem(settingsKey)||'null');}catch{}if(position?.open&&$('modalLayer').hidden){settingsRestoring=true;try{const [kind,id]=position.scope.split(':');await PhoenixSettings.open({section:position.section,scope:kind==='global'?{kind}:{kind,id}});$('settingsMain').scrollTop=position.scroll||0;}finally{settingsRestoring=false;}}}
  // Reconcile semantic empty/history state without waiting for compositor
  // frames. Native occlusion can pause RAF after a renderer-only reload.
  let layoutQueued=false,layoutActive=true;
  const observer=new MutationObserver(()=>{if(layoutQueued)return;layoutQueued=true;queueMicrotask(()=>{layoutQueued=false;if(!layoutActive)return;projectCommentary();layout()})});observer.observe(feed,{childList:true,subtree:true});observer.observe($('composerContext'),{childList:true,subtree:true});observer.observe($('contextPercent'),{childList:true,subtree:true});projectCommentary();layout();
  // The original shell measures its toolbar below separate window chrome.
  // Phoenix puts that chrome in a 34px header. Measure the actual bottom,
  // rather than reusing a hard-coded row count that lets a native page cover it.
  let boundsFrame=0;
  function alignBrowser(){boundsFrame=0;const toolbar=document.querySelector('#inspectionSidebar .inspection-toolbar'),bar=document.querySelector('#inspectionSidebar .inspection-tabbar');if(!toolbar||!bar)return;const bottom=Math.max(bar.getBoundingClientRect().bottom,toolbar.getBoundingClientRect().bottom),next=Math.round(bottom)+'px';if(bottom>0&&document.documentElement.style.getPropertyValue('--mc-browser-top')!==next){document.documentElement.style.setProperty('--mc-browser-top',next);PhoenixConversation.refreshPanelBounds()}}
  const queueBounds=()=>{if(!boundsFrame)boundsFrame=requestAnimationFrame(alignBrowser)},boundsObserver=new ResizeObserver(queueBounds);
  for(const node of document.querySelectorAll('#inspectionSidebar,.inspection-toolbar,.inspection-tabbar,#teachingTopbar'))boundsObserver.observe(node);
  addEventListener('phoenix:inspection-tab',queueBounds);addEventListener('resize',queueBounds);queueBounds();
  addEventListener('pagehide',()=>{boundsObserver.disconnect();cancelAnimationFrame(boundsFrame);removeEventListener('resize',queueBounds);removeEventListener('phoenix:inspection-tab',queueBounds)},{once:true});
  addEventListener('pagehide',()=>{layoutActive=false;observer.disconnect();animation?.cancel()},{once:true});void restoreLocalForms();document.documentElement.dataset.reviewReady='true';document.title=root.__PHOENIX_ISOLATED_BACKEND__?.enabled?'Phoenix':'Phoenix';
 }
 if(ready())init();else{const timer=setInterval(()=>{if(ready()){clearInterval(timer);init()}},50);addEventListener('pagehide',()=>clearInterval(timer),{once:true})}
})(globalThis);
