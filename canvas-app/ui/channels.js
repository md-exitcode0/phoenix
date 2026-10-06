"use strict";
(() => {
  const ui=window.PhoenixUI;if(!ui)return;
  const esc=ui.escapeHtml,preview=!ui.TAURI||ui.SIDEBAR_PREVIEW;
  let workspace="",generation=0,dialogGeneration=0,modalCleanup=()=>{},modalReturnTarget=null;
  const recoveryPageSize=10;
  const call=(action,args={})=>preview?Promise.reject(new Error("Open Phoenix desktop to connect a real chat.")):ui.invoke("channels_command",{action,id:null,config:null,token:null,value:null,kind:null,offset:null,limit:null,snapshot:null,...args});
  function failure(host,error) {
    host.replaceChildren();
    const line=document.createElement('p');line.setAttribute('role','alert');line.textContent="Could not finish this channel action.";
    const detail=document.createElement('details'),summary=document.createElement('summary'),body=document.createElement('pre');summary.textContent="Details";body.textContent=String(error?.message||error).slice(0,2000);detail.append(summary,body);host.append(line,detail);
  }
  function agents(){return (ui.state.view?.directory?.agents||[]).filter(a=>a.canonical_session_id&&!['deleted','archived','pending_deletion'].includes(a.lifecycle));}
  function status(row){
    const s=row.status||{},d=s.delivery||{},attention=row.worker_error||s.error||d.needs_review||d.uncertain_replies||d.rejected_replies;
    if(d.capacity?.paused)return `${!s.running&&!row.desktop_worker?'Disconnected · ':''}Intake paused${attention?' · Needs attention':''}`;
    if(attention)return 'Needs attention';
    if(s.running){if(d.sending_replies)return `${d.running?'Agent working · ':''}${d.sending_images?'Sending image':'Sending reply'}`;return d.running?'Agent working':d.queued?'Messages queued':'Connected';}
    return row.desktop_worker?'Starting…':'Disconnected';
  }
  function channelControl(host,id){return [...host.querySelectorAll('[data-channel-id]')].find(button=>button.dataset.channelId===id)||host.querySelector('[data-channel-add],[data-channel-refresh]');}
  function focusChannel(host,id){if(host.isConnected&&document.getElementById('modalLayer').hidden)channelControl(host,id)?.focus();}
  async function render(host){
    if(!host)return;const revision=++generation;
    const active=document.activeElement,restoreRefresh=host.contains(active)&&active.matches('[data-channel-refresh]'),modalRevision=dialogGeneration;
    host.innerHTML='<p class="settings-section-note">Loading channels…</p>';
    try {
      const result=preview?{connections:[],workspace:""}:await call('list');
      if(revision!==generation||!host.isConnected)return;
      const rows=result.connections||[];workspace=result.workspace||"";
      host.innerHTML=`<div class="channel-intro"><span><h3>Talk to your coworkers from Telegram or Discord</h3><ol class="channel-howto"><li><span><b>Create a bot</b> (with @BotFather in Telegram, or in the Discord Developer Portal) and paste its token.</span></li><li><span><b>Say hi</b> to your bot, in a private chat or a group. Phoenix finds the chat and your user ID by itself.</span></li><li><span><b>Pick a coworker or group</b> to answer. The chat and its conversation in Phoenix mirror each other, and it shows “typing…” while it works.</span></li></ol></span><span class="channel-intro-actions"><button type="button" class="button primary" data-channel-telegram>Set up Telegram</button><button type="button" class="button secondary" data-channel-discord>Set up Discord</button><button type="button" class="text-action" data-channel-add>Enter IDs manually</button></span></div>${preview?'<p class="settings-section-note">Preview only · use the desktop app to connect a real chat.</p>':''}<div class="channel-list">${rows.map((r,i)=>r.config?`<article class="channel-card"><div><strong>${esc(r.config.name)}</strong><span class="channel-status">${esc(status(r))}</span><p>${esc(r.config.platform==='telegram'?'Telegram':'Discord')} · ${esc(targetName(r.config))} · ${esc(r.config.conversation_id)}</p>${r.status?.delivery?.notice?`<p class="channel-notice">${esc(r.status.delivery.notice)}</p>`:''}${r.worker_error?`<p class="channel-notice">${esc(r.worker_error)}</p>`:''}</div><button type="button" class="button secondary" data-channel-manage="${i}">Manage</button></article>`:`<p role="alert">${esc(r.error)}</p>`).join('')||'<p class="channel-empty">No channels connected yet.</p>'}</div><div class="channel-footer"><span>Keep Phoenix running to receive messages.</span><button type="button" class="button secondary" data-channel-refresh>Refresh status</button></div><div data-channel-error></div>`;
      host.querySelector('[data-channel-add]').onclick=()=>edit(host);
      host.querySelector('[data-channel-telegram]').onclick=()=>telegramSetup(host);
      host.querySelector('[data-channel-discord]').onclick=()=>discordSetup(host);
      host.querySelector('[data-channel-refresh]').onclick=()=>render(host);
      host.querySelectorAll('[data-channel-manage]').forEach(b=>{const row=rows[Number(b.dataset.channelManage)];b.dataset.channelId=row.config.id;b.onclick=()=>manage(host,row);});
      return result;
    } catch(error){if(revision===generation&&host.isConnected){failure(host,error);const retry=document.createElement('button');retry.type='button';retry.className='button secondary';retry.dataset.channelRefresh='';retry.textContent='Refresh status';retry.onclick=()=>render(host);host.append(retry);}}
    finally{if(restoreRefresh&&revision===generation&&modalRevision===dialogGeneration&&host.isConnected&&document.activeElement===document.body&&document.getElementById('modalLayer').hidden)host.querySelector('[data-channel-refresh]')?.focus();}
    return null;
  }
  function visibleControl(node){
    const closed=node.closest('details:not([open])');
    return !node.disabled && node.getClientRects().length && (!closed || closed.querySelector('summary')===node);
  }
  function dialog(title,copy,body,focusTarget){
    const opener=document.activeElement,revision=++dialogGeneration;modalCleanup();
    const returnTo=focusTarget||(opener?.closest('.channel-dialog')?modalReturnTarget:()=>opener);modalReturnTarget=returnTo;
    ui.showModal(`<section class="modal channel-dialog" role="dialog" aria-modal="true" aria-labelledby="channelTitle"><header class="modal-header"><span><strong id="channelTitle" tabindex="-1">${esc(title)}</strong><small>${esc(copy)}</small></span><button type="button" class="modal-close" aria-label="Close">×</button></header>${body}<div class="channel-error" data-dialog-error></div></section>`);
    const layer=document.getElementById('modalLayer'),dialog=layer.querySelector('.channel-dialog');
    function restore(){if(revision!==dialogGeneration||!layer.hidden)return;const target=returnTo?.();if(target?.isConnected)target.focus();}
    function closing(){if(dialog.isConnected)queueMicrotask(restore);modalCleanup();}
    layer.addEventListener('phoenix:modal-closing',closing);modalCleanup=()=>layer.removeEventListener('phoenix:modal-closing',closing);
    function close(){ui.closeModal();restore();}
    dialog.querySelector('.modal-close').onclick=close;
    dialog.addEventListener('input',event=>{if(event.target.setCustomValidity){event.target.setCustomValidity('');event.target.removeAttribute('aria-invalid');}});
    dialog.addEventListener('invalid',event=>{for(let parent=event.target.parentElement;parent&&parent!==dialog;parent=parent.parentElement)if(parent.tagName==='DETAILS')parent.open=true;},true);
    dialog.addEventListener('keydown',event=>{
      if(event.key==='Escape'){event.preventDefault();event.stopPropagation();close();return;}
      if(event.key==='Tab'){const nodes=[...dialog.querySelectorAll('button,input,select,textarea,summary,a[href]')].filter(visibleControl),first=nodes[0],last=nodes.at(-1),active=document.activeElement;
        if(!nodes.includes(active)||(event.shiftKey?active===first:active===last)){event.preventDefault();(event.shiftKey?last:first)?.focus();}}
    });
    (dialog.querySelector('[data-has-recovery]')?dialog.querySelector('#channelTitle'):([...dialog.querySelectorAll('input,select')].find(visibleControl)||dialog.querySelector('[data-connect]')||dialog.querySelector('button')))?.focus();return dialog;
  }
  async function operation(dialog,fn){
    if(!dialog.isConnected||dialog.dataset.busy)return;dialog.dataset.busy='true';
    const focused=document.activeElement,buttons=[...dialog.querySelectorAll('button:not(.modal-close),input,select,textarea')],disabled=buttons.map(b=>b.disabled);
    dialog.querySelector('[data-dialog-error]').replaceChildren();buttons.forEach(b=>b.disabled=true);dialog.setAttribute('aria-busy','true');
    try{await fn();}catch(error){if(dialog.isConnected)failure(dialog.querySelector('[data-dialog-error]'),error);}finally{
      delete dialog.dataset.busy;dialog.removeAttribute('aria-busy');buttons.forEach((b,i)=>b.disabled=disabled[i]);
      if(dialog.isConnected&&dialog.contains(focused)&&!focused.disabled&&(document.activeElement===document.body||document.activeElement===dialog))focused.focus({preventScroll:true});
    }
  }
  async function closeAndRefresh(modal,host,id){
    if(!modal.isConnected)return;const revision=dialogGeneration;ui.closeModal();await render(host);if(revision===dialogGeneration)focusChannel(host,id);
  }
  function fieldValidity(input,message){input.setCustomValidity(message);if(message)input.setAttribute('aria-invalid','true');else input.removeAttribute('aria-invalid');}
  function reportValidity(form){
    const invalid=[...form.elements].find(input=>input.willValidate&&!input.validity.valid);
    if(invalid){for(let parent=invalid.parentElement;parent;parent=parent.parentElement)if(parent.tagName==='DETAILS')parent.open=true;}
    return form.reportValidity();
  }
  function edit(host,row){
    const c=row?.config,options=agents();
    const form=dialog(c?'Edit channel':'Add channel','Choose who can message an agent and where its replies go.',`<form class="channel-form"><label>Name<input name="name" maxlength="80" required value="${esc(c?.name||'')}" placeholder="My Telegram"></label><div class="channel-fields"><label>Service<select name="platform" ${c?'disabled':''}><option value="telegram" ${c?.platform==='telegram'?'selected':''}>Telegram</option><option value="discord" ${c?.platform==='discord'?'selected':''}>Discord</option></select></label><label>Agent<select name="agent" ${c?'disabled':''} required>${options.map(a=>`<option value="${esc(a.agent_id)}" ${(c?.agent_id||ui.state.selected?.id)===a.agent_id?'selected':''}>${esc(a.display_name||a.agent_id)}</option>`).join('')}</select></label></div><label>Chat or channel ID<input name="destination" required pattern="-?[0-9]+" ${c?'readonly':''} value="${esc(c?.conversation_id||'')}" placeholder="123456789"></label><label>Allowed user IDs<input name="users" required value="${esc(c?.allowed_user_ids?.join(', ')||'')}" placeholder="123456789, 987654321"><small>Only these people can give this agent instructions.</small></label><details ${c?'':'open'}><summary>Setup help</summary><p>Telegram: create a bot with BotFather and use a private chat. Discord: add your bot, allow Message Content, and copy the channel and user IDs using Developer Mode.</p><input type="hidden" name="workspace" value="${esc(c?.workspace||workspace)}"><p class="channel-help-links"><a href="https://core.telegram.org/bots/tutorial" target="_blank" rel="noreferrer">Telegram setup ↗</a><a href="https://docs.discord.com/developers/quick-start/getting-started" target="_blank" rel="noreferrer">Discord setup ↗</a></p></details><footer class="modal-footer"><button type="submit" class="button primary" ${!options.length?'disabled':''}>Save channel</button></footer></form>`);
    form.querySelector('form').onsubmit=event=>{
      event.preventDefault();if(form.dataset.busy)return;
      const element=event.currentTarget,f=element.elements,agent=c?{agent_id:c.agent_id,canonical_session_id:c.session_id}:options.find(a=>a.agent_id===f.agent.value);
      const name=f.name.value.trim(),platform=c?.platform||f.platform.value,destination=f.destination.value.trim(),users=f.users.value.split(/[\s,]+/).filter(Boolean),path=f.workspace.value.trim();
      fieldValidity(f.name,name&&[...name].length<=80?'':'Enter a name using 1–80 characters.');
      fieldValidity(f.destination,(platform==='telegram'?/^-?[0-9]{1,20}$/:/^[0-9]{1,20}$/).test(destination)?'':`Enter a ${platform==='telegram'?'chat':'channel'} ID using up to 20 digits${platform==='telegram'?', with an optional leading minus sign':''}.`);
      fieldValidity(f.users,users.length&&users.every(id=>/^[0-9]{1,20}$/.test(id))?'':'Enter user IDs using up to 20 digits each, separated by spaces or commas.');
      fieldValidity(f.workspace,path.startsWith('/')?'':'Enter an absolute workspace path, starting with /.');
      if(!reportValidity(element))return;
      if(!agent){failure(form.querySelector('[data-dialog-error]'),Error('Choose an available agent'));return;}
      const config={id:c?.id||`chat-${crypto.randomUUID()}`,name,platform,agent_id:agent.agent_id,session_id:agent.canonical_session_id,conversation_id:destination,allowed_user_ids:users,workspace:path,enabled:c?.enabled??true,token_env:c?.token_env||'PHOENIX_CHANNEL_BOT_TOKEN'};
      operation(form,async()=>{await call('save',{config});await closeAndRefresh(form,host,config.id);});
    };
  }
  function loginControls(saved){
    const field=`<label>${saved?'New bot token':'Bot token'}<input type="password" name="token" autocomplete="off" spellcheck="false" placeholder="Paste your bot token"><small>${saved?'Replacing the login leaves the channel settings unchanged.':'Use it once, or remember it in your encrypted vault.'}</small></label>`;
    const connect='<button type="button" class="button secondary" data-check>Test login</button><button type="button" class="button primary" data-connect>Connect</button>';
    const remember=`<button type="button" class="button secondary" data-remember>${saved?'Save replacement':'Remember login'}</button>`;
    return saved?`<p class="channel-login-saved"><strong>Bot login saved</strong><br><small>Stored in your encrypted vault. Ready to reconnect.</small></p><div class="channel-actions">${connect}</div><details><summary>Replace or forget login</summary><div class="channel-login-replace">${field}<div class="channel-actions">${remember}<button type="button" class="button secondary" data-forget>Forget saved token</button></div></div></details><p data-login-result role="status"></p>`:`${field}<div class="channel-actions">${connect}${remember}</div><p data-login-result role="status"></p>`;
  }
  async function refreshManage(host,modal,id,view){
    const result=await render(host);if(!modal.isConnected)return;
    if(!result){
      modal.querySelector('.modal-header small').textContent='Action completed · refresh status';
      modal.querySelector('.channel-form').innerHTML='<p role="status">The action finished, but channel status could not be refreshed. Refresh status before continuing.</p><button type="button" class="button secondary" data-recovery-refresh>Refresh status</button>';
      const retry=modal.querySelector('[data-recovery-refresh]');retry.onclick=()=>operation(modal,()=>refreshManage(host,modal,id,view));retry.focus();return;
    }
    const row=result.connections?.find(item=>item.config?.id===id);
    if(!row){ui.closeModal();focusChannel(host,id);return;}
    const next=manage(host,row,view);
    if(view.focusRecovery){const target=next.querySelector('[data-recovery-range]')||next.querySelector('#channelTitle');target.focus({preventScroll:true});next.querySelector('.channel-recovery-nav')?.scrollIntoView({block:'nearest'});}
  }
  function recovery(modal,host,row,view){
    const root=modal.querySelector('.channel-recovery'),d=row.status?.delivery||{};
    const remote=d.review_paging===true;
    // Full arrays are retained only for older fixtures/backends. The native API
    // advertises counts and explicitly serves one snapshot-bound page at a time.
    const groups={deliveries:Array.isArray(d.review_deliveries)?d.review_deliveries:[],turns:Array.isArray(d.review_turns)?d.review_turns:[]};
    let counts=remote?{...d.review_counts}:{deliveries:groups.deliveries.length,turns:groups.turns.length};
    if(!Object.values(counts).some(count=>count>0))return;
    if(!counts[view.kind])view.kind=counts.deliveries?'deliveries':'turns';
    let running=Boolean(row.status?.running||row.desktop_worker),pageRecords=[],snapshot=null,unavailable=false;
    const labels={deliveries:'Replies to check',turns:'Turns to review'};
    root.innerHTML=`<div class="channel-recovery-heading"><h3>Review saved work</h3>${remote?'<button type="button" class="button secondary" data-recovery-refresh>Refresh</button>':''}</div><p data-recovery-help></p><p data-recovery-running ${running?'':'hidden'}>Disconnect before resolving a reply or reviewing a turn.</p><div class="channel-recovery-toolbar"><select data-recovery-kind aria-label="Choose replies or interrupted turns"></select><span data-recovery-range role="status" aria-live="polite" tabindex="-1"></span></div><div class="channel-recovery-nav"><button type="button" class="button secondary" data-recovery-prev aria-label="Previous page">←</button><form class="channel-recovery-jump"><label>Page<input type="number" min="1" step="1" required data-recovery-page></label><span data-recovery-pages></span><button type="submit" class="button secondary">Go</button></form><button type="button" class="button secondary" data-recovery-next aria-label="Next page">→</button></div><div class="channel-recovery-items" data-recovery-items role="list"></div>`;
    const select=root.querySelector('[data-recovery-kind]'),previous=root.querySelector('[data-recovery-prev]'),next=root.querySelector('[data-recovery-next]'),range=root.querySelector('[data-recovery-range]'),page=root.querySelector('[data-recovery-page]'),items=root.querySelector('[data-recovery-items]'),nav=root.querySelector('.channel-recovery-nav');
    const reload=root.querySelector('[data-recovery-refresh]');
    function syncControls(){
      if(!modal.isConnected)return;
      previous.disabled=unavailable||view.page<=1;next.disabled=unavailable||view.page>=Math.ceil((counts[view.kind]||0)/recoveryPageSize);
      select.disabled=unavailable;page.disabled=unavailable;page.form.querySelector('button').disabled=unavailable;
      if(unavailable)reload?.focus();
      else if(root.contains(document.activeElement)&&document.activeElement.disabled)range.focus({preventScroll:true});
    }
    function paint(reveal=false){
      const total=counts[view.kind]||0,pages=Math.max(1,Math.ceil(total/recoveryPageSize)),active=document.activeElement;
      view.page=Math.max(1,Math.min(Number.isInteger(view.page)?view.page:1,pages));
      const start=(view.page-1)*recoveryPageSize,visible=remote?pageRecords:groups[view.kind].slice(start,start+recoveryPageSize);
      select.innerHTML=Object.entries(counts).filter(([,count])=>count>0).map(([kind,count])=>`<option value="${kind}">${labels[kind]} (${count.toLocaleString()})</option>`).join('');
      select.value=view.kind;page.max=String(pages);page.value=String(view.page);page.setCustomValidity('');previous.disabled=view.page===1;next.disabled=view.page===pages;
      range.textContent=total?`${start+1}–${start+visible.length} of ${total.toLocaleString()}`:'No items left to review';root.querySelector('[data-recovery-pages]').textContent=`of ${pages.toLocaleString()}`;
      root.querySelector('[data-recovery-running]').hidden=!running;
      root.querySelector('[data-recovery-help]').textContent=view.kind==='turns'?'Check each interrupted turn in Phoenix before continuing queued messages.':`Check ${row.config.platform==='telegram'?'Telegram':'Discord'} before resolving each reply. Resending a reply that arrived can duplicate it.`;
      items.innerHTML=visible.map((record,index)=>{
        const turn=view.kind==='turns',id=turn?record.turn_id:record.id,valid=typeof id==='string'&&/^[A-Za-z0-9_]+$/.test(id),disabled=running||!valid?'disabled':'',description=`channelRecoveryId${index}`;
        const button=(action,label)=>`<button type="button" class="button ${action==='retry'?'secondary':'primary'}" data-recover="${action}" data-value="${esc(id||'')}" aria-describedby="${description}" ${disabled}>${label}</button>`;
        const preview=record.preview,text=typeof preview?.text==='string'?preview.text:'',heading=text||`${turn?'Interrupted turn':'Saved reply'} ${start+index+1}`;
        const type=({message:'Incoming message',text:'Text reply',question:'Question',image:'Image reply'})[preview?.kind]||'';
        return `<article class="channel-recovery-item" role="listitem"><details data-recovery-record ${index===0?'open':''}><summary><span class="channel-recovery-number">${start+index+1}</span><span class="channel-recovery-copy"><strong>${esc(heading)}${preview?.truncated?'…':''}</strong><span class="channel-recovery-state">${esc(({needs_review:'Interrupted turn',running:'Agent working',uncertain:'Delivery uncertain',rejected:'Delivery rejected',sending:'Sending reply'})[record.state]||'Needs review')}${type?` · ${esc(type)}`:''}</span></span><span class="channel-recovery-chevron" aria-hidden="true">⌄</span></summary><div class="channel-recovery-detail">${preview?.truncated?'<small>Excerpt from the saved content. Open Phoenix for the full conversation.</small>':''}<div class="channel-actions">${turn?button('acknowledge','I reviewed this turn'):button('received','It arrived')+button('retry','Not received · retry')}</div><details class="channel-reference"><summary>Reference ID</summary><code id="${description}">${esc(id||'ID unavailable — refresh status before continuing.')}</code></details></div></details></article>`;
      }).join('');
      items.querySelectorAll('[data-recovery-record]').forEach(details=>details.addEventListener('toggle',()=>{
        if(details.open)items.querySelectorAll('[data-recovery-record]').forEach(other=>{if(other!==details)other.open=false;});
      }));
      items.querySelectorAll('[data-recover]').forEach(button=>{
        const action=button.dataset.recover,value=button.dataset.value,expectedSnapshot=snapshot;
        button.onclick=()=>operation(modal,async()=>{
          await call(action,{id:row.config.id,value,snapshot:expectedSnapshot});if(!modal.isConnected)return;
          const message=action==='received'?'Reply marked as received.':action==='retry'?'Reply queued for retry. Connect to send it.':'Turn review saved. Connect to continue queued messages.';
          await refreshManage(host,modal,row.config.id,{...view,message,focusRecovery:true});
        });
      });
      if(reveal){
        const target=modal.contains(active)&&!active.disabled?active:range;
        if(document.activeElement!==target)target.focus({preventScroll:true});
        (target===select?select:nav).scrollIntoView({block:'nearest'});
      }
    }
    function requestPage(targetPage,targetKind,expected=snapshot,reveal=true){
      if(modal.dataset.busy||!modal.isConnected)return;
      if(!remote){view.page=targetPage;view.kind=targetKind;paint(reveal);return;}
      const offset=(targetPage-1)*recoveryPageSize;
      operation(modal,async()=>{
        try {
          const result=await call('review',{id:row.config.id,kind:targetKind,offset,limit:recoveryPageSize,snapshot:expected});
          if(!modal.isConnected)return;
          const total=result?.total,list=result?.items,nextOffset=offset+(Array.isArray(list)?list.length:0),nextCounts=result?.delivery?.review_counts;
          const validCounts=nextCounts&&['deliveries','turns'].every(key=>Number.isSafeInteger(nextCounts[key])&&nextCounts[key]>=0);
          const valid=validCounts&&result.id===row.config.id&&result.kind===targetKind&&result.offset===offset&&result.limit===recoveryPageSize&&
            Number.isSafeInteger(total)&&total>=0&&nextCounts[targetKind]===total&&Array.isArray(list)&&list.length===Math.min(recoveryPageSize,Math.max(0,total-offset))&&
            (offset<total||(offset===0&&total===0))&&result.next_offset===(nextOffset<total?nextOffset:null)&&typeof result.running==='boolean'&&
            typeof result.snapshot==='string'&&/^[a-f0-9]{64}$/.test(result.snapshot)&&(!expected||result.snapshot===expected)&&
            !result.delivery.review_deliveries&&!result.delivery.review_turns&&list.every(record=>{
              const id=targetKind==='turns'?record.turn_id:record.id;
              const preview=record.preview,validPreview=preview===undefined||(preview&&typeof preview.text==='string'&&[...preview.text].length<=200&&typeof preview.truncated==='boolean'&&['message','text','question','image'].includes(preview.kind));
              return validPreview&&typeof id==='string'&&id.length<=128&&/^[A-Za-z0-9_]+$/.test(id)&&(targetKind==='turns'?['needs_review','running']:['uncertain','rejected','sending']).includes(record.state);
            })&&new Set(list.map(record=>targetKind==='turns'?record.turn_id:record.id)).size===list.length;
          if(!valid||new TextEncoder().encode(JSON.stringify(result)).length>=256*1024)throw Error('Phoenix returned an invalid recovery page. Refresh the list before continuing.');
          counts={deliveries:nextCounts.deliveries,turns:nextCounts.turns};pageRecords=list;snapshot=result.snapshot;unavailable=false;
          running=Boolean(result.running||row.desktop_worker);view.kind=targetKind;view.page=targetPage;paint(reveal);
        } catch(error){
          if(modal.isConnected){unavailable=true;pageRecords=[];items.replaceChildren();range.textContent='Recovery list needs refreshing.';}
          throw error;
        }
      }).then(syncControls);
    }
    select.onchange=()=>{if(!modal.dataset.busy&&!unavailable&&counts[select.value])requestPage(1,select.value);};
    previous.onclick=()=>{if(!unavailable)requestPage(view.page-1,view.kind);};
    next.onclick=()=>{if(!unavailable)requestPage(view.page+1,view.kind);};
    page.form.onsubmit=event=>{
      event.preventDefault();if(modal.dataset.busy)return;const value=Number(page.value);page.setCustomValidity('');
      if(!Number.isInteger(value)||value<1||value>Number(page.max))page.setCustomValidity(`Enter a page from 1 to ${page.max}.`);
      if(!page.form.reportValidity()||unavailable)return;requestPage(value,view.kind);
    };
    if(reload)reload.onclick=()=>requestPage(1,view.kind,null);
    if(remote){view.page=Math.max(1,Math.min(Number.isInteger(view.page)?view.page:1,Math.ceil(counts[view.kind]/recoveryPageSize)));requestPage(view.page,view.kind,null,false);}
    else paint();
  }
  function manage(host,row,savedView={}){
    const c=row.config,d=row.status?.delivery||{},running=Boolean(row.status?.running||row.desktop_worker),view={...savedView};
    const hasRecovery=d.review_paging?((d.review_counts?.turns||0)+(d.review_counts?.deliveries||0)>0):(d.review_turns?.length||0)+(d.review_deliveries?.length||0)>0;
    const notice=d.notice||(d.capacity?.paused?'New messages and work are paused. Existing work is retained. Keep this channel connected to send saved replies. Do not resubmit pending work.':'');
    const activeCopy=d.capacity?.paused?'':row.status?.running?'<p>Receiving messages from the people you allowed. Disconnecting leaves agent work available in Phoenix.</p>':'<p>Starting this connection. Check its status again to confirm it is receiving messages.</p>';
    const connection=running?`${activeCopy}<button type="button" class="button secondary" data-disconnect ${!row.desktop_worker?'disabled':''}>Disconnect</button>${!row.desktop_worker?'<p>Started outside this desktop. Disconnect it in its terminal.</p>':''}`:loginControls(row.status?.saved_login);
    const noticeHtml=notice?(hasRecovery?`<details class="channel-capacity-notice"><summary>${d.capacity?.paused?'Intake paused · saved work is retained':'Connection notice'}</summary><p>${esc(notice)}</p></details>`:`<p class="channel-capacity-notice" role="status">${esc(notice)}</p>`):'';
    const modal=dialog(c.name,`${c.platform==='telegram'?'Telegram':'Discord'} · ${status(row)}`,`<div class="channel-form">${noticeHtml}${view.message?`<p class="channel-recovery-result" role="status">${esc(view.message)}</p>`:''}${!hasRecovery||running?connection:''}<section class="channel-recovery" ${hasRecovery?'data-has-recovery':''}></section>${hasRecovery&&!running?`<details class="channel-connection-controls"><summary>Connection &amp; bot login</summary><div class="channel-login-replace">${connection}</div></details>`:''}<details class="channel-connection-settings"><summary>Connection settings</summary><p>${esc(c.conversation_id)} · ${c.allowed_user_ids.length} allowed people</p><div class="channel-actions"><button type="button" class="button secondary" data-edit ${running?'disabled':''}>Edit name &amp; access</button><button type="button" class="button secondary" data-remove ${running?'disabled':''}>Remove connection</button></div><small>Delivered history is retained. Resolve pending work before removal.</small></details></div>`,()=>channelControl(host,c.id));
    const act=(selector,action)=>modal.querySelector(selector)?.addEventListener('click',()=>{
      if(modal.dataset.busy||!modal.isConnected)return;
      const input=modal.querySelector('[name=token]'),token=input?.value.trim()||null;
      if(input){
        const missing=!token&&(action==='remember'||(!row.status?.saved_login&&action!=='forget'));
        fieldValidity(input,missing?'Enter your bot token first.':'');
        if(missing){for(let parent=input.parentElement;parent&&parent!==modal;parent=parent.parentElement)if(parent.tagName==='DETAILS')parent.open=true;input.reportValidity();return;}
      }
      operation(modal,async()=>{
        await call(action,{id:c.id,token});
        // Remember a deliberate Disconnect so startup does not undo it.
        try{if(action==='stop')localStorage.setItem(`phoenix-channel-stopped:${c.id}`,'1');if(action==='start')localStorage.removeItem(`phoenix-channel-stopped:${c.id}`);}catch{}
        if(!modal.isConnected)return;
        if(input&&['start','remember'].includes(action))input.value='';
        if(action==='remember'||action==='forget'){await refreshManage(host,modal,c.id,view);return;}
        if(action==='check'){modal.querySelector('[data-login-result]').textContent='Bot login verified. Ready to connect.';return;}
        await closeAndRefresh(modal,host,c.id);
      });
    });
    act('[data-check]','check');act('[data-connect]','start');act('[data-disconnect]','stop');act('[data-remember]','remember');act('[data-forget]','forget');
    modal.querySelector('[data-edit]')?.addEventListener('click',()=>edit(host,row));
    modal.querySelector('[data-remove]')?.addEventListener('click',()=>{
      const confirm=dialog('Remove this connection?',`Messages from ${c.name} will no longer reach Phoenix.`,`<footer class="modal-footer"><button type="button" class="button danger" data-confirm-remove>Remove connection</button></footer>`);
      confirm.querySelector('[data-confirm-remove]').onclick=()=>operation(confirm,async()=>{await call('remove',{id:c.id});await closeAndRefresh(confirm,host,c.id);});
    });
    recovery(modal,host,row,view);return modal;
  }

  // Where a chat's messages go: one coworker, or a Phoenix group.
  function targets(){
    const groups=(ui.state.view?.directory?.groups||[]).filter(g=>g.canonical_session_id&&(!g.lifecycle||g.lifecycle==='active'));
    return {agents:agents(),groups};
  }
  function targetField(){
    const {agents:list,groups}=targets();
    return `<label>Who answers<select name="target"><optgroup label="Coworkers">${list.map(a=>`<option value="agent:${esc(a.agent_id)}">${esc(a.display_name)}</option>`).join('')}</optgroup>${groups.length?`<optgroup label="Groups">${groups.map(g=>`<option value="group:${esc(g.group_id)}">${esc(g.name)}</option>`).join('')}</optgroup>`:''}</select></label>`;
  }
  function targetConfig(value){
    const [kind,id]=String(value||'').split(/:(.*)/s),{agents:list,groups}=targets();
    if(kind==='group'){const g=groups.find(x=>x.group_id===id);return g&&{label:g.name,agent_id:g.group_id,session_id:g.canonical_session_id,group_id:g.group_id};}
    const a=list.find(x=>x.agent_id===id);return a&&{label:a.display_name,agent_id:a.agent_id,session_id:a.canonical_session_id,group_id:null};
  }
  function targetName(config){
    if(config.group_id)return (ui.state.view?.directory?.groups||[]).find(g=>g.group_id===config.group_id)?.name||config.group_id;
    return agents().find(a=>a.agent_id===config.agent_id)?.display_name||config.agent_id;
  }
  function setupShell(title,subtitle,steps,finishLabel){
    return dialog(title,subtitle,`<div class="channel-form tg-setup">${steps.map((step,i)=>`<section class="tg-step" data-tg-step="${i+1}" ${i?'aria-disabled="true"':''}><header><i>${i+1}</i><span><strong>${step.title}</strong><small>${step.help}</small></span></header>${step.body}</section>`).join('')}
      <section class="tg-step" data-tg-step="${steps.length+1}" aria-disabled="true"><header><i>${steps.length+1}</i><span><strong>Choose who answers</strong><small>Messages from this chat go to this coworker or group, and show up in its chat in Phoenix. What you write to it in Phoenix is mirrored back here.</small></span></header>
        ${targetField()}
        <label class="tg-check"><input type="checkbox" name="remember" checked><span>Remember the bot login on this computer (encrypted)</span></label>
        <details class="tg-advanced"><summary>Details</summary><label>Chat ID<input name="destination" inputmode="numeric" placeholder="Found automatically"></label><label>Allowed user IDs<input name="users" placeholder="Found automatically"></label></details>
        <div class="tg-row tg-finish"><button type="button" class="button primary" data-tg-connect disabled>${finishLabel}</button></div></section></div>`);
  }
  function wizardKit(modal){
    const q=(sel)=>modal.querySelector(sel),f=(name)=>modal.querySelector(`[name=${name}]`);
    const openLink=(url)=>{try{const p=ui.invoke?.('open_external',{url});if(p?.catch)p.catch(()=>window.open(url,'_blank'));else if(!p)window.open(url,'_blank');}catch{window.open(url,'_blank');}};
    const unlock=(n,on=true)=>q(`[data-tg-step="${n}"]`)?.setAttribute('aria-disabled',String(!on));
    const error=(message)=>{const box=q('[data-dialog-error]');box.replaceChildren();if(message){const p=document.createElement('p');p.setAttribute('role','alert');p.textContent=message;box.append(p);}};
    const guarded=(fn)=>()=>operation(modal,async()=>{try{await fn();}catch(e){error(e?.message||String(e));}});
    return {q,f,openLink,unlock,error,guarded};
  }
  async function finishSetup(host,modal,{platform,name,token,destination,users,remember,target}){
    const t=targetConfig(target);
    if(!t)throw new Error('Choose who answers.');
    if(!workspace)throw new Error('Phoenix has not reported its workspace yet. Close this and try again.');
    const config={id:`chat-${crypto.randomUUID()}`,name,platform,agent_id:t.agent_id,session_id:t.session_id,conversation_id:destination,allowed_user_ids:users,workspace,enabled:true,token_env:'PHOENIX_CHANNEL_BOT_TOKEN',...(t.group_id?{group_id:t.group_id}:{})};
    await call('save',{config});
    if(remember)await call('remember',{id:config.id,token});
    await call('start',{id:config.id,token});
    await closeAndRefresh(modal,host,config.id);
    return t.label;
  }

  // ── Telegram ───────────────────────────────────────────────────────────
  // The bot token only ever goes to api.telegram.org (to name the bot and
  // find the chat) and to Phoenix's own channel worker.
  const tgApi=async(token,method,params={})=>{
    const url=new URL(`https://api.telegram.org/bot${token}/${method}`);Object.entries(params).forEach(([k,v])=>url.searchParams.set(k,String(v)));
    let body;try{body=await (await fetch(url)).json();}catch{throw new Error('Could not reach Telegram. Check your internet connection.');}
    if(!body.ok){
      if(body.error_code===401||body.error_code===404)throw new Error('Telegram does not recognise this token. Copy the whole token from BotFather (it looks like 123456789:AA…).');
      if(body.error_code===409)throw new Error('This bot is already connected somewhere else (another app is reading its messages). Disconnect it there, then try again.');
      throw new Error(body.description||'Telegram refused the request.');
    }
    return body.result;
  };
  const personName=(u)=>[u.first_name,u.last_name].filter(Boolean).join(' ')||u.username||u.global_name||String(u.id);
  function telegramSetup(host){
    const modal=setupShell('Set up Telegram','Works in a private chat or a group. Takes about two minutes.',[
      {title:'Create your bot',help:'In Telegram, open <b>@BotFather</b>, send <code>/newbot</code>, choose a name and a username ending in “bot”. BotFather replies with a token.',
        body:`<div class="tg-row"><button type="button" class="button secondary" data-open-botfather>Open @BotFather</button></div><label>Bot token<input name="token" type="password" autocomplete="off" spellcheck="false" placeholder="123456789:AA…"></label><div class="tg-row"><button type="button" class="button primary" data-check>Check token</button><span class="tg-result" data-bot role="status"></span></div>`},
      {title:'Say hi to your bot',help:'<b>Private chat:</b> open your bot and send it any message, like “hi”.<br><b>Group:</b> add the bot to the group, then send a message that mentions it (for example “@yourbot hi”). To let it read every message, make the bot an admin, or in @BotFather send <code>/setprivacy</code> and choose Disable.',
        body:`<div class="tg-row"><button type="button" class="button secondary" data-open-bot disabled>Open your bot</button><span class="tg-result" data-listen role="status"></span></div><div class="tg-chats" data-chats></div>`},
    ],'Connect Telegram');
    const {q,f,openLink,unlock,error,guarded}=wizardKit(modal);
    let bot=null,token='',listening=0;const found=new Map();
    q('[data-open-botfather]').onclick=()=>openLink('https://t.me/BotFather');
    q('[data-open-bot]').onclick=()=>bot&&openLink(`https://t.me/${bot.username}`);
    const pick=(id)=>{const chat=found.get(id);if(!chat)return;
      modal.querySelectorAll('[data-chat]').forEach(b=>b.setAttribute('aria-pressed',String(b.dataset.chat===id)));
      f('destination').value=chat.id;f('users').value=[...chat.users.keys()].join(', ');unlock(3);q('[data-tg-connect]').disabled=false;};
    const paint=()=>{q('[data-chats]').innerHTML=[...found.values()].map(chat=>`<button type="button" class="tg-chat" data-chat="${esc(String(chat.id))}" aria-pressed="false"><strong>${esc(chat.type==='private'?personName(chat):chat.title||'Group')}</strong><small>${chat.type==='private'?'Private chat':'Group'} · can give instructions: ${[...chat.users.values()].map(esc).join(', ')}</small></button>`).join('');
      modal.querySelectorAll('[data-chat]').forEach(b=>b.onclick=()=>pick(b.dataset.chat));
      if(found.size===1)pick(String([...found.keys()][0]));};
    async function listen(){
      const run=++listening,started=Date.now(),status=q('[data-listen]');
      status.textContent='Waiting for your message…';
      while(run===listening&&modal.isConnected&&Date.now()-started<300000){
        try{
          const updates=await tgApi(token,'getUpdates',{timeout:0,allowed_updates:'["message"]'});let last=0,before=found.size;
          for(const u of updates){last=Math.max(last,u.update_id);const m=u.message;if(!m?.chat)continue;
            // A group that became a supergroup gets a new ID; follow it.
            const chatId=String(m.migrate_to_chat_id||m.chat.id);
            if(!m.from||m.from.is_bot){continue;}
            const chat=found.get(chatId)||{...m.chat,id:chatId,users:new Map()};chat.users.set(String(m.from.id),personName(m.from));found.set(chatId,chat);}
          // Mark them read so the "hi" isn't delivered to the coworker later.
          if(last)await tgApi(token,'getUpdates',{offset:last+1,timeout:0}).catch(()=>{});
          if(found.size!==before){status.textContent=found.size===1?'Found your chat.':'Found these chats. Pick one.';paint();}
        }catch(e){status.textContent='';error(e.message);return;}
        await new Promise(r=>setTimeout(r,2500));
      }
      if(run===listening&&modal.isConnected&&!found.size){status.innerHTML='No message yet. <button type="button" class="text-action" data-retry>Check again</button>';q('[data-retry]').onclick=listen;}
    }
    q('[data-check]').onclick=guarded(async()=>{
      error('');token=f('token').value.trim();
      if(!/^\d{5,}:[\w-]{20,}$/.test(token))throw new Error('Paste the whole token from BotFather. It looks like 123456789:AA…');
      bot=await tgApi(token,'getMe');
      q('[data-bot]').textContent=`✓ Your bot is @${bot.username}`;q('[data-open-bot]').disabled=false;q('[data-open-bot]').textContent=`Open @${bot.username}`;
      unlock(2);listen();
    });
    f('token').addEventListener('input',()=>{bot=null;listening++;found.clear();q('[data-bot]').textContent='';q('[data-listen]').textContent='';q('[data-chats]').innerHTML='';unlock(2,false);unlock(3,false);q('[data-tg-connect]').disabled=true;q('[data-open-bot]').disabled=true;});
    q('[data-tg-connect]').onclick=guarded(async()=>{
      error('');const destination=f('destination').value.trim(),users=f('users').value.split(/[\s,]+/).filter(Boolean);
      if(!bot||!token)throw new Error('Check your bot token first.');
      if(!/^-?\d{1,20}$/.test(destination)||!users.length||!users.every(id=>/^\d{1,20}$/.test(id)))throw new Error('Send your bot a message first so Phoenix can find the chat.');
      listening++;
      const chat=found.get(destination),name=chat&&chat.type!=='private'?`Telegram · ${chat.title||'group'}`:`Telegram · @${bot.username}`;
      const who=await finishSetup(host,modal,{platform:'telegram',name,token,destination,users,remember:f('remember').checked,target:f('target').value});
      token='';ui.toast?.(`Telegram connected. Messages to @${bot.username} now reach ${who}.`);
    });
    return modal;
  }

  // ── Discord ────────────────────────────────────────────────────────────
  const dcApi=async(token,path)=>{
    let response;try{response=await fetch(`https://discord.com/api/v10${path}`,{headers:{Authorization:`Bot ${token}`}});}catch{throw new Error('Could not reach Discord. Check your internet connection.');}
    if(response.status===401)throw new Error('Discord does not recognise this token. In the Developer Portal open your app → Bot → Reset Token, and paste the new one.');
    if(response.status===403)throw new Error('The bot cannot see that channel. Give it the View Channel and Read Message History permissions there.');
    if(!response.ok)throw new Error(`Discord refused the request (HTTP ${response.status}).`);
    return response.json();
  };
  function discordSetup(host){
    const modal=setupShell('Set up Discord','Talk to a coworker in a Discord channel. Takes about three minutes.',[
      {title:'Create your bot',help:'Open the Discord Developer Portal → <b>New Application</b> → <b>Bot</b>. Turn on <b>Message Content Intent</b>, click <b>Reset Token</b> and copy the token.',
        body:`<div class="tg-row"><button type="button" class="button secondary" data-open-portal>Open Developer Portal</button></div><label>Bot token<input name="token" type="password" autocomplete="off" spellcheck="false" placeholder="Paste the bot token"></label><div class="tg-row"><button type="button" class="button primary" data-check>Check token</button><span class="tg-result" data-bot role="status"></span></div>`},
      {title:'Invite it and pick a channel',help:'Add the bot to your server, then choose the channel where you will talk to it.',
        body:`<div class="tg-row"><button type="button" class="button secondary" data-invite disabled>Invite the bot to a server</button><button type="button" class="text-action" data-reload-servers disabled>I invited it, refresh</button></div><div class="channel-fields"><label>Server<select name="guild" disabled><option value="">Invite the bot first</option></select></label><label>Channel<select name="channel" disabled><option value="">Choose a server</option></select></label></div>`},
      {title:'Say hi in that channel',help:'Send any message in the channel. Phoenix sees who wrote it, and only those people can give instructions.',
        body:`<div class="tg-row"><span class="tg-result" data-listen role="status"></span></div><div class="tg-chats" data-people></div>`},
    ],'Connect Discord');
    const {q,f,openLink,unlock,error,guarded}=wizardKit(modal);
    let bot=null,token='',listening=0;const people=new Map();
    q('[data-open-portal]').onclick=()=>openLink('https://discord.com/developers/applications');
    // View Channels + Send Messages + Attach Files + Read Message History.
    q('[data-invite]').onclick=()=>bot&&openLink(`https://discord.com/oauth2/authorize?client_id=${bot.id}&scope=bot&permissions=101376`);
    async function loadServers(){
      const guilds=await dcApi(token,'/users/@me/guilds'),select=f('guild');
      select.innerHTML=guilds.length?`<option value="">Choose a server</option>${guilds.map(g=>`<option value="${esc(g.id)}">${esc(g.name)}</option>`).join('')}`:'<option value="">Invite the bot first</option>';
      select.disabled=!guilds.length;if(guilds.length===1){select.value=guilds[0].id;await loadChannels();}
    }
    async function loadChannels(){
      const id=f('guild').value,select=f('channel');listening++;people.clear();q('[data-people]').innerHTML='';unlock(3,false);unlock(4,false);
      if(!id){select.innerHTML='<option value="">Choose a server</option>';select.disabled=true;return;}
      const channels=(await dcApi(token,`/guilds/${id}/channels`)).filter(c=>c.type===0).sort((a,b)=>(a.position||0)-(b.position||0));
      select.innerHTML=`<option value="">Choose a channel</option>${channels.map(c=>`<option value="${esc(c.id)}">#${esc(c.name)}</option>`).join('')}`;select.disabled=false;
    }
    async function listen(){
      const run=++listening,channel=f('channel').value,started=Date.now(),status=q('[data-listen]');
      if(!channel)return;unlock(3);status.textContent='Waiting for your message…';
      const since=BigInt(Date.now()-1420070400000-60000)<<22n;
      while(run===listening&&modal.isConnected&&Date.now()-started<300000){
        try{
          const messages=await dcApi(token,`/channels/${channel}/messages?limit=20`),before=people.size;
          for(const m of messages){if(m.author?.bot||m.webhook_id||BigInt(m.id)<since)continue;people.set(String(m.author.id),personName(m.author));}
          if(people.size!==before){
            q('[data-people]').innerHTML=[...people].map(([id,name])=>`<label class="tg-chat tg-person"><input type="checkbox" value="${esc(id)}" checked><span><strong>${esc(name)}</strong><small>can give instructions</small></span></label>`).join('');
            const sync=()=>{const ids=[...modal.querySelectorAll('.tg-person input:checked')].map(i=>i.value);f('users').value=ids.join(', ');q('[data-tg-connect]').disabled=!ids.length;};
            modal.querySelectorAll('.tg-person input').forEach(i=>i.onchange=sync);
            f('destination').value=channel;sync();unlock(4);status.textContent='Found you.';
          }
        }catch(e){status.textContent='';error(e.message);return;}
        await new Promise(r=>setTimeout(r,3000));
      }
    }
    q('[data-check]').onclick=guarded(async()=>{
      error('');token=f('token').value.trim();
      if(token.length<50)throw new Error('Paste the whole bot token from the Developer Portal (Bot → Reset Token).');
      bot=await dcApi(token,'/users/@me');
      if(!bot.bot)throw new Error('That is a user token. Use the token from your app’s Bot page.');
      q('[data-bot]').textContent=`✓ Your bot is ${bot.username}`;q('[data-invite]').disabled=false;q('[data-reload-servers]').disabled=false;
      unlock(2);await loadServers();
    });
    q('[data-reload-servers]').onclick=guarded(loadServers);
    f('guild').onchange=guarded(loadChannels);
    f('channel').onchange=()=>{people.clear();q('[data-people]').innerHTML='';listen();};
    f('token').addEventListener('input',()=>{bot=null;listening++;people.clear();q('[data-bot]').textContent='';[2,3,4].forEach(n=>unlock(n,false));q('[data-tg-connect]').disabled=true;});
    q('[data-tg-connect]').onclick=guarded(async()=>{
      error('');const destination=f('destination').value.trim(),users=f('users').value.split(/[\s,]+/).filter(Boolean);
      if(!bot||!token)throw new Error('Check your bot token first.');
      if(!/^\d{1,20}$/.test(destination)||!users.length)throw new Error('Say hi in the channel first so Phoenix can find you.');
      listening++;
      const channelName=f('channel').selectedOptions[0]?.textContent||'channel';
      const who=await finishSetup(host,modal,{platform:'discord',name:`Discord · ${channelName}`,token,destination,users,remember:f('remember').checked,target:f('target').value});
      token='';ui.toast?.(`Discord connected. Messages in ${channelName} now reach ${who}.`);
    });
    return modal;
  }
  // After Phoenix restarts, reconnect every chat whose login is saved,
  // unless the user disconnected it on purpose.
  async function reconnectSaved(){
    if(preview)return;
    try{
      const rows=(await call('list')).connections||[];
      for(const row of rows){
        const id=row.config?.id;let stopped=false;try{stopped=localStorage.getItem(`phoenix-channel-stopped:${id}`)==='1';}catch{}
        if(!id||!row.config.enabled||!row.status?.saved_login||row.status?.running||row.desktop_worker||stopped)continue;
        await call('start',{id}).catch(()=>{});
      }
    }catch{}
  }
  setTimeout(reconnectSaved,4000);
  window.PhoenixChannels=Object.freeze({render});
})();
