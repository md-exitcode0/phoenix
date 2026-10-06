/* Synthetic API only. The UI and its styles are the production component. */
"use strict";
(() => {
  const host=document.getElementById('channels'),layer=document.getElementById('modalLayer');
  let connections=[],held=[],failures=[],paged=false,revision=0,reviewTransform=null;
  const calls=[],errors=[],responses=[];
  const notice='Channel reply capacity is in use. New messages and work are paused; existing work is retained. Keep this channel connected to send saved replies. Do not resubmit pending work.';
  const clone=value=>structuredClone(value);
  const examplePreviews=[
    {kind:'text',text:'Here is the revised project plan. The three open decisions are listed at the end.'},
    {kind:'question',text:'Which version should I use for the final document: the short summary or the full report?'},
    {kind:'image',text:'project-timeline.png'},
    {kind:'text',text:'The comparison is ready. I highlighted the differences and kept the original source links.'},
    {kind:'text',text:'Your weekly notes are organized by project. Two items still need your review.'}
  ];
  const revisionFor=target=>`${(connections.indexOf(target)+1).toString(16).padStart(16,'0')}${revision.toString(16).padStart(48,'0')}`;
  function summary(target){
    const {review_deliveries,review_turns,...delivery}=target.status.delivery;
    return {...target,status:{...target.status,delivery:{...delivery,review_paging:true,
      review_counts:{deliveries:review_deliveries.length,turns:review_turns.length}}}};
  }
  function pageResult(args){
    const target=connections.find(item=>item.config.id===args.id);
    if(!target)throw Error('Connection missing');
    if(!['turns','deliveries'].includes(args.kind)||!Number.isSafeInteger(args.offset)||args.offset<0||!Number.isSafeInteger(args.limit)||args.limit<1||args.limit>100)throw Error('Invalid review page');
    const snapshot=revisionFor(target);
    if(args.snapshot&&args.snapshot!==snapshot)throw Error('Recovery snapshot changed. Refresh the recovery list.');
    const items=target.status.delivery[args.kind==='turns'?'review_turns':'review_deliveries'],total=items.length;
    if(args.offset>=total&&(args.offset!==0||total!==0))throw Error('Review page is out of range. Refresh the recovery list.');
    const selected=clone(items.slice(args.offset,args.offset+args.limit)),end=args.offset+selected.length;
    return {id:args.id,kind:args.kind,offset:args.offset,limit:args.limit,snapshot,total,items:selected,
      next_offset:end<total?end:null,running:target.status.running,delivery:clone(summary(target).status.delivery)};
  }
  function response(action,value){
    if(paged)responses.push({action,bytes:new TextEncoder().encode(JSON.stringify(value)).length,
      items:action==='review'?value.items.length:undefined,hasArrays:action==='list'&&value.connections.some(row=>row.status.delivery.review_deliveries||row.status.delivery.review_turns)});
    return value;
  }
  function row(id='fixture_telegram',{running=false,paused=false,deliveries=0,turns=0,saved=true}={}) {
    return {config:{id,name:id==='fixture_telegram'?'My Telegram':'Project Discord',platform:id==='fixture_telegram'?'telegram':'discord',agent_id:'avery',session_id:'agent_avery',conversation_id:'18446744073709551615',allowed_user_ids:['123','456'],workspace:'/tmp/channel-fixture',enabled:true,token_env:'UNUSED_FIXTURE_TOKEN'},desktop_worker:running,status:{running,saved_login:saved,delivery:{
      capacity:{paused,serialized_bytes:paused?66000000:1024,limit_bytes:67108864,active_observers:0,result_reserve_bytes:1048576},notice:paused?notice:null,
      queued:0,running:0,pending_replies:paused?200:0,uncertain_replies:deliveries,rejected_replies:0,needs_review:turns,
      review_deliveries:Array.from({length:deliveries},(_,i)=>({id:`fixture_delivery_${String(i+1).padStart(5,'0')}`,state:'uncertain',preview:{...examplePreviews[i%examplePreviews.length],truncated:false}})),
      review_turns:Array.from({length:turns},(_,i)=>({turn_id:`fixture_turn_${String(i+1).padStart(5,'0')}`,state:'needs_review',preview:{kind:'message',text:'Please review the updated project plan and identify anything that still needs a decision.',truncated:false}}))
    }}};
  }
  function validate(config) {
    if(!config.name.trim())throw Error('Name must contain 1–80 characters');
    if(!(config.platform==='telegram'?/^-?[0-9]{1,20}$/:/^[0-9]{1,20}$/).test(config.conversation_id))throw Error('Invalid conversation ID');
    if(!config.allowed_user_ids.length||config.allowed_user_ids.some(id=>!/^[0-9]{1,20}$/.test(id)))throw Error('Choose at least one allowed user ID');
    if(!config.workspace.startsWith('/'))throw Error('Workspace must be an existing absolute directory');
  }
  function closeModal(reason='dismiss') {
    if(layer.hidden)return;
    const normalized=typeof reason==='string'?reason:'dismiss';
    layer.dispatchEvent(new CustomEvent('phoenix:modal-closing',{detail:{reason:normalized}}));
    layer.hidden=true;layer.innerHTML='';
    queueMicrotask(()=>window.dispatchEvent(new CustomEvent('phoenix:modal-visibility',{detail:{open:false,reason:normalized}})));
  }
  window.PhoenixUI={TAURI:true,state:{selected:{id:'avery'},view:{directory:{agents:[{agent_id:'avery',canonical_session_id:'agent_avery',display_name:'Avery'},{agent_id:'phoenix',canonical_session_id:'agent_phoenix',display_name:'Phoenix'}]}}},
    escapeHtml:s=>String(s).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c])),
    showModal(html){layer.hidden=false;layer.innerHTML=html;window.dispatchEvent(new CustomEvent('phoenix:modal-visibility',{detail:{open:true}}));layer.querySelector('.modal-close')?.addEventListener('click',closeModal);layer.onpointerdown=event=>{if(event.target===layer)closeModal();};},closeModal,
    async invoke(command,args) {
      if(command!=='channels_command')throw Error('Unexpected command');
      calls.push(clone(args));
      const snapshot=args.action==='list'?clone({connections:paged?connections.map(summary):connections,workspace:'/tmp/channel-fixture'}):args.action==='review'?pageResult(args):null;
      const gate=held.find(item=>item.action===args.action&&!item.taken);
      if(gate){gate.taken=true;await gate.promise;}
      const failure=failures.findIndex(item=>item.action===args.action);
      if(failure>=0)throw Error(failures.splice(failure,1)[0].message);
      if(args.action==='list')return response('list',snapshot);
      if(args.action==='review'){
        const transform=reviewTransform;reviewTransform=null;
        return response('review',transform?transform(snapshot):snapshot);
      }
      if(args.action==='save'){
        validate(args.config);
        const old=connections.find(item=>item.config.id===args.config.id);
        if(old)old.config=clone(args.config);else connections.push({...row(args.config.id),config:clone(args.config)});
        return {id:args.config.id};
      }
      const target=connections.find(item=>item.config.id===args.id);
      if(!target)throw Error('Connection missing');
      if(args.action==='remember')target.status.saved_login=true;
      if(args.action==='forget')target.status.saved_login=false;
      if(args.action==='start'){target.status.running=true;target.desktop_worker=true;}
      if(args.action==='stop'){target.status.running=false;target.desktop_worker=false;}
      if(args.action==='remove')connections=connections.filter(item=>item!==target);
      if(['received','retry','acknowledge'].includes(args.action)){
        if(target.status.running)throw Error('Disconnect before recovery');
        if(paged&&args.snapshot!==revisionFor(target))throw Error('Recovery snapshot changed. Refresh the recovery list.');
        const turns=args.action==='acknowledge',key=turns?'review_turns':'review_deliveries',idKey=turns?'turn_id':'id',items=target.status.delivery[key];
        const index=items.findIndex(item=>item[idKey]===args.value);
        if(index<0)throw Error('No unresolved item with this exact ID');
        items.splice(index,1);
        target.status.delivery[turns?'needs_review':'uncertain_replies']=items.length;
        if(args.action==='retry')target.status.delivery.pending_replies++;
        revision++;
      }
      return {ok:true};
    }
  };
  window.ChannelFixture={host,calls,errors,responses,row,notice,escapeBubbles:0,
    async set(next){closeModal();connections=clone(next);revision++;calls.length=0;responses.length=0;failures=[];reviewTransform=null;return PhoenixChannels.render(host);},
    async scenario(mode){return this.set(mode==='empty'?[]:mode==='large'?[row('fixture_telegram',{paused:true,deliveries:2047,turns:1033})]:mode==='login'?[row()]:[row('fixture_telegram',{running:true,paused:true})]);},
    data:()=>clone(connections),mutate:fn=>{fn(connections);revision++;},
    setPaged(value){paged=Boolean(value);},transformReview(fn){reviewTransform=fn;},
    fail(action,message='Fixture connection error'){failures.push({action,message});},
    hold(action){let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});const gate={action,promise,resolve,reject,taken:false};held.push(gate);return gate;},
    manage(index=0){const button=host.querySelectorAll('[data-channel-manage]')[index];if(!button)throw Error('Manage is unavailable');button.focus();button.click();return document.querySelector('.channel-dialog');}
  };
  window.addEventListener('keydown',event=>{if(event.key==='Escape')ChannelFixture.escapeBubbles++;});
  window.addEventListener('error',event=>errors.push(event.message));
  window.addEventListener('unhandledrejection',event=>errors.push(String(event.reason)));
})();
