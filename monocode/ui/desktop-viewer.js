(() => {
  const select=document.getElementById('desktopWorkspacePicker');
  const image=document.getElementById('desktopObservationImage');
  const empty=document.getElementById('desktopObservationEmpty');
  const caption=document.getElementById('desktopObservationCaption');
  const status=document.getElementById('desktopWorkspaceStatus');
  if(!select||!image||!empty||!caption||!status)return;
  let visible=false,context='',rpc=null,generation=0,timer=0,controller=null;
  let ownerScoped=false,owners=[],ownerSignature='';
  let selected='',captured=null,signature='',pending=false;
  const clearImage=()=>{image.removeAttribute('src');image.hidden=true;captured=null;caption.textContent='Latest observation';};
  const stage=document.getElementById('desktopObservationStage'),connecting=document.getElementById('desktopConnecting'),fullButton=document.getElementById('desktopFullscreen'),cursor=document.getElementById('desktopCursor');
  // Waiting for pixels shows the reference's connecting screen (black,
  // spinner ring, one line); errors and empty states stay as text.
  const WAITING=/^Loading/;
  const message=text=>{const waiting=WAITING.test(text);connecting.hidden=!waiting;empty.hidden=waiting;empty.textContent=text;stage.classList.toggle('connecting',waiting);stage.classList.add('no-frame');caption.hidden=true;};
  const shown=()=>{connecting.hidden=true;stage.classList.remove('connecting','no-frame');caption.hidden=false;};
  // Full screen: the button in the top-right corner zooms the agent's screen
  // to the whole display; the same spot (or Esc) zooms back into Phoenix.
  const isFull=()=>document.fullscreenElement===stage||stage.classList.contains('fullscreen');
  const syncFull=()=>{const full=isFull();stage.classList.toggle('is-full',full);fullButton.setAttribute('aria-label',full?'Exit full screen':'Full screen');fullButton.title=full?'Exit full screen':'Full screen';if(!full)cursor.hidden=true;};
  fullButton.addEventListener('click',async(event)=>{event.stopPropagation();try{if(isFull()){if(document.fullscreenElement)await document.exitFullscreen();stage.classList.remove('fullscreen');}else if(stage.requestFullscreen){await stage.requestFullscreen();}else stage.classList.add('fullscreen');}catch{stage.classList.toggle('fullscreen');}syncFull();});
  document.addEventListener('fullscreenchange',syncFull);
  document.addEventListener('keydown',(event)=>{if(event.key!=='Escape'||!isFull())return;event.preventDefault();if(document.fullscreenElement===stage)void document.exitFullscreen().catch(()=>{});stage.classList.remove('fullscreen');syncFull();});
  stage.addEventListener('pointermove',(event)=>{if(!isFull()||event.target.closest('#desktopFullscreen')){cursor.hidden=true;return;}const box=stage.getBoundingClientRect();cursor.hidden=false;cursor.style.transform=`translate(${event.clientX-box.left}px, ${event.clientY-box.top}px)`;});
  stage.addEventListener('pointerleave',()=>{cursor.hidden=true;});
  const fresh=g=>visible&&!document.hidden&&g===generation;
  const stop=()=>{clearTimeout(timer);controller?.abort();controller=null;};
  function invalidate(){generation++;stop();pending=false;}
  function identity(value){
    // Runtime labels are compact internal ids (school_coach), while company
    // profiles can expose the same owner as “School Coach” or “school-coach”.
    // Compare the semantic id rather than punctuation so owner-scoped desktop
    // lists do not silently drop a coworker's screen.
    return String(value||'').trim().toLowerCase()
      .replace(/[\s_-]+/g,'_').replace(/[^a-z0-9_]/g,'')
      .replace(/^_+|_+$/g,'').slice(0,32);
  }
  function normalizedOwners(value){
    if(!Array.isArray(value))return[];
    return value.map((owner,index)=>{
      const aliases=[owner?.id,owner?.label,...(Array.isArray(owner?.aliases)?owner.aliases:[])].map(identity).filter(Boolean);
      return{id:String(owner?.id||''),label:String(owner?.label||owner?.id||'Coworker'),aliases:[...new Set(aliases)],index};
    }).filter(owner=>owner.aliases.length);
  }
  function workspaceOwner(workspace){
    if(workspace?.owner_agent_id)return identity(workspace.owner_agent_id);
    return identity(String(workspace?.label||'').split(' · ')[0]);
  }
  function ownerForWorkspace(workspace){
    const candidate=workspaceOwner(workspace);
    return owners.find(owner=>owner.aliases.includes(candidate))||null;
  }
  function visibleWorkspaces(all){
    if(!ownerScoped)return all;
    return all.filter(workspace=>ownerForWorkspace(workspace)).sort((left,right)=>{
      const a=ownerForWorkspace(left)?.index??Number.MAX_SAFE_INTEGER,b=ownerForWorkspace(right)?.index??Number.MAX_SAFE_INTEGER;
      return a-b||String(left.label||'').localeCompare(String(right.label||''));
    });
  }
  function workspaceLabel(workspace){
    const owner=ownerForWorkspace(workspace);if(!owner)return workspace.label||'Agent desktop';
    const raw=String(workspace.label||''),parts=raw.split(' · '),suffix=parts.length>1?parts.slice(1).join(' · ').trim():'';
    return suffix?`${owner.label} · ${suffix}`:owner.label;
  }
  function ownersWithoutDesktop(workspaces){
    if(!ownerScoped)return[];
    const present=new Set(workspaces.map(workspace=>ownerForWorkspace(workspace)?.id).filter(Boolean));
    return owners.filter(owner=>!present.has(owner.id));
  }
  async function refresh(){
    if(!visible||document.hidden||pending||!rpc)return;
    const g=generation;pending=true;controller=new AbortController();
    try{
      const result=await rpc('DesktopWorkspaces',8000,controller.signal);
      if(!fresh(g))return;
      const inventory=result.DesktopWorkspaces;
      if(!Array.isArray(inventory))throw Error('Workspace viewing needs an updated gateway.');
      const workspaces=visibleWorkspaces(inventory);
      const waitingOwners=ownersWithoutDesktop(workspaces);
      const next=JSON.stringify([
        workspaces.map(w=>[w.scope_key,workspaceLabel(w),w.running,w.in_use]),
        waitingOwners.map(owner=>[owner.id,owner.label]),
      ]);
      if(next!==signature){
        signature=next;select.replaceChildren();
        for(const workspace of workspaces){
          const option=document.createElement('option');option.value=workspace.scope_key;
          option.textContent=workspaceLabel(workspace);select.append(option);
        }
        for(const owner of waitingOwners){
          const option=document.createElement('option');option.value=`pending:${owner.id}`;option.disabled=true;
          option.textContent=`${owner.label} · No desktop yet`;select.append(option);
        }
        if(!workspaces.length&&!waitingOwners.length){const option=document.createElement('option');option.textContent='No open desktop';option.value='';select.append(option);}
        if(!workspaces.some(w=>w.scope_key===selected)){selected=workspaces[0]?.scope_key||'';clearImage();}
        if(selected)select.value=selected;
      }
      select.disabled=!workspaces.length;
      if(!selected){status.textContent='';clearImage();message(ownerScoped?'A desktop will appear here when this coworker or group member opens an app.':'An agent’s screen will appear here when it opens an app.');return;}
      const workspace=workspaces.find(w=>w.scope_key===selected);
      status.textContent=!workspace?.running?'Stopped':workspace.in_use?'Working':'Available';
      const reply=await rpc({DesktopObservation:{scope_key:selected,after_ms:captured}},8000,controller.signal);
      if(!fresh(g))return;
      const frame=reply.DesktopObservation;
      if(frame?.scope_key!==selected)throw Error('The workspace changed before its image arrived.');
      // A recreated scope can have no observation yet. A missing capture is
      // different from an unchanged capture whose pixels were omitted.
      if(frame.captured_at_ms==null){clearImage();message('Agent screen not loaded yet');return;}
      if(frame.data_url){
        if(!frame.data_url.startsWith('data:image/png;base64,'))throw Error('The workspace image could not be displayed.');
        image.src=frame.data_url;image.alt=`Latest observed screen from ${workspaceLabel(workspace)}`;
        image.hidden=false;empty.hidden=true;shown();captured=frame.captured_at_ms;
      }
      if(captured)image.alt=`Latest observed screen from ${workspaceLabel(workspace)}`;
      if(captured){
        const seconds=Math.max(0,Math.floor((Date.now()-captured)/1000));
        const age=seconds<5?'just now':seconds<60?`${seconds}s ago`:`${Math.floor(seconds/60)}m ago`;
        const dimensions=Number(frame.width)>0&&Number(frame.height)>0?` · ${frame.width}×${frame.height}`:'';
        caption.textContent=`Latest ${frame.kind==='window'?'window':'desktop'} observation${dimensions} · ${age}`;
      }else message('Agent screen not loaded yet');
    }catch(error){
      if(fresh(g)&&error.name!=='AbortError'){status.textContent='Unavailable';clearImage();message(error.message||'Could not load this workspace.');}
    }finally{
      if(g===generation){pending=false;controller=null;if(visible&&!document.hidden)timer=setTimeout(refresh,1500);}
    }
  }
  select.addEventListener('change',()=>{invalidate();selected=select.value;clearImage();message('Loading observation…');refresh();});
  document.addEventListener('visibilitychange',()=>{invalidate();if(visible&&!document.hidden)refresh();});
  image.addEventListener('error',()=>{
    // A failed decode is not an observed frame. Fetch the pixels again on
    // the next poll instead of asking only for images newer than this one.
    clearImage();status.textContent='Unavailable';
    message('The screen image could not be decoded. Trying again.');
  });
  window.PhoenixDesktopViewer={
    update(options){
      const nextOwners=normalizedOwners(options.owners),nextOwnerScoped=Array.isArray(options.owners),nextOwnerSignature=JSON.stringify(nextOwners.map(owner=>[owner.id,owner.label,owner.aliases]));
      const changed=context!==options.context||ownerScoped!==nextOwnerScoped||ownerSignature!==nextOwnerSignature;
      rpc=options.rpc;
      if(visible===Boolean(options.visible)&&!changed)return;
      invalidate();visible=Boolean(options.visible);context=options.context||'';ownerScoped=nextOwnerScoped;owners=nextOwners;ownerSignature=nextOwnerSignature;
      if(changed){selected='';signature='';select.replaceChildren();clearImage();message('Loading workspaces…');}
      if(visible)refresh();
    }
  };
})();
