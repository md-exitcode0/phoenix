/* Run in recovery.html, using the production component and the synthetic API. */
"use strict";
window.runChannelChecks=async function runChannelChecks() {
  const F=ChannelFixture,results=[];
  const modal=()=>document.querySelector('.channel-dialog');
  const get=(selector,root=modal())=>{const node=root?.querySelector(selector);if(!node)throw Error(`Missing control: ${selector}`);return node;};
  const assert=(ok,message,actual)=>{if(!ok)throw Error(`${message}${actual===undefined?'':`: ${JSON.stringify(actual)}`}`);};
  const equal=(actual,expected,message)=>{
    const nodes=actual instanceof Node||expected instanceof Node;
    assert(nodes?actual===expected:JSON.stringify(actual)===JSON.stringify(expected),message,nodes?{actual:actual?.outerHTML?.slice(0,300),expected:expected?.outerHTML?.slice(0,300)}:{actual,expected});
  };
  const settle=()=>new Promise(resolve=>setTimeout(resolve,0));
  const until=async(fn)=>{for(let i=0;i<100;i++){if(fn())return;await new Promise(resolve=>setTimeout(resolve,5));}throw Error('Component did not reach expected state');};
  const actions=()=>F.calls.filter(call=>!['list','check'].includes(call.action));
  const click=(selector,root)=>{const node=get(selector,root);node.focus();node.click();return node;};
  const close=()=>click('.modal-close');
  const codes=()=>[...modal().querySelectorAll('.channel-recovery code')].map(node=>node.textContent);
  const range=()=>get('[data-recovery-range]').textContent;
  const goto=async(page)=>{const input=get('[data-recovery-page]');input.value=String(page);input.form.requestSubmit();await settle();};
  const choose=async(kind)=>{const select=get('[data-recovery-kind]');select.value=kind;select.dispatchEvent(new Event('change',{bubbles:true}));await settle();};
  const test=async(name,fn)=>{try{const evidence=await fn();results.push({name,passed:true,...(evidence?{evidence}:{})});}catch(error){results.push({name,passed:false,error:error.message});}finally{await settle();}};
  async function addForm(values={}) {
    await F.set([]);click('[data-channel-add]',F.host);
    const form=get('form'),defaults={name:'My channel',platform:'telegram',agent:'avery',destination:'123',users:'456',workspace:'/tmp/channel-fixture',...values};
    for(const [key,value] of Object.entries(defaults)){const input=form.elements.namedItem(key);input.value=value;input.dispatchEvent(new Event('input',{bubbles:true}));}
    return form;
  }

  await test('Capacity pause is visible on both card and Manage',async()=>{
    await F.scenario('capacity');F.manage();
    const card=F.host.querySelector('.channel-card').innerText,body=modal().innerText;
    assert(/paused/i.test(F.host.querySelector('.channel-status').textContent),'Card claims normal intake',card);
    assert(!/Receiving messages/i.test(body),'Manage claims it is receiving while intake is paused',body);
    assert(body.includes(F.notice),'Manage omits the capacity recovery instructions',body);
    return {card,body};
  });
  await test('Disconnected capacity pause keeps reconnect guidance',async()=>{
    await F.set([F.row('fixture_telegram',{paused:true})]);F.manage();
    assert(/Disconnected/i.test(F.host.querySelector('.channel-status').textContent),'Disconnected status is hidden');
    assert(/paused/i.test(F.host.querySelector('.channel-status').textContent),'Paused status is hidden');
    assert(get('[data-connect]')&&!/Receiving messages/.test(modal().innerText),'Reconnect is unavailable');
  });
  await test('Sending image remains distinct when intake is active',async()=>{
    const row=F.row('fixture_telegram',{running:true});Object.assign(row.status.delivery,{running:1,sending_replies:1,sending_images:1});
    await F.set([row]);assert(/Agent working.*Sending image/.test(F.host.innerText),'Sending image status lost',F.host.innerText);
  });
  await test('Large arrays are bounded and their last records remain reachable',async()=>{
    await F.scenario('large');F.manage();
    const measured={renderedRecords:codes().length,renderedActions:modal().querySelectorAll('[data-recover]').length,scrollHeight:modal().scrollHeight,deliveries:2047,turns:1033};
    assert(measured.renderedRecords<=10,'Recovery renders the whole backlog',measured);
    assert(get('[data-recovery-kind]').textContent.replaceAll(',','').includes('2047'),'Total delivery count is missing');
    await goto(205);assert(codes().includes('fixture_delivery_02047'),'Last delivery is unreachable',codes());
    await choose('turns');await goto(104);assert(codes().includes('fixture_turn_01033'),'Last turn is unreachable',codes());
    equal(F.data()[0].status.delivery.review_deliveries.length,2047,'Pagination discarded deliveries');
    equal(F.data()[0].status.delivery.review_turns.length,1033,'Pagination discarded turns');
    equal(actions().length,0,'Browsing recovery changed backend records');return measured;
  });
  await test('Invalid page jumps do not change page or send actions',async()=>{
    await F.set([F.row('fixture_telegram',{deliveries:27})]);F.manage();await goto(2);const before=range();
    for(const value of ['0','4','1.5','']){await goto(value);equal(range(),before,'Invalid page jump changed selection');}
    equal(actions().length,0,'Page navigation called a mutation');
  });
  await test('Exact reply identity survives double click, reordered refresh and retry',async()=>{
    await F.set([F.row('fixture_telegram',{deliveries:27,turns:3})]);F.manage();await goto(2);
    const selected=get('[data-recover="received"][data-value="fixture_delivery_00013"]'),gate=F.hold('received');
    try{
      selected.focus();selected.click();selected.click();await until(()=>gate.taken);
      equal(actions().length,1,'Double click sent duplicate actions');
      F.mutate(rows=>rows[0].status.delivery.review_deliveries.reverse());gate.resolve();
      await until(()=>modal()?.querySelector('[data-recovery-range]')?.textContent.includes('26'));
      equal(actions()[0].value,'fixture_delivery_00013','Action selected a different delivery');
      const saved=F.data()[0].status.delivery.review_deliveries;
      equal(saved.length,26,'Resolution changed more than one record');assert(!saved.some(item=>item.id==='fixture_delivery_00013'),'Selected reply remains unresolved');
      const retry=get('[data-recover="retry"]'),retryId=retry.dataset.value;retry.click();
      await until(()=>actions().length===2&&modal()?.querySelector('[data-recovery-range]')?.textContent.includes('25'));
      equal(actions().map(call=>({action:call.action,id:call.id,value:call.value})),[{action:'received',id:'fixture_telegram',value:'fixture_delivery_00013'},{action:'retry',id:'fixture_telegram',value:retryId}],'Wrong exact action receipt');
      return {receipts:actions(),remaining:F.data()[0].status.delivery.review_deliveries.length};
    }finally{gate.resolve();}
  });
  await test('Resolving the only record on the last page preserves earlier records',async()=>{
    await F.set([F.row('fixture_telegram',{deliveries:11})]);F.manage();await goto(2);
    click('[data-recover="received"]');await until(()=>modal()?.querySelector('[data-recovery-range]')?.textContent.includes('of 10'));
    equal(codes().length,10,'Last-page clamp failed');equal(get('[data-recovery-page]').value,'1','Page did not clamp');
    equal(actions()[0].value,'fixture_delivery_00011','Wrong final-page delivery');
  });
  await test('Same recovery IDs in two channels retain the chosen channel and turn',async()=>{
    await F.set([F.row('fixture_telegram',{deliveries:4,turns:3}),F.row('fixture_discord',{deliveries:4,turns:3})]);F.manage(1);await choose('turns');
    click('[data-recover="acknowledge"][data-value="fixture_turn_00002"]');await until(()=>F.data()[1].status.delivery.review_turns.length===2);
    equal(F.data()[0].status.delivery.review_turns.length,3,'Other channel was mutated');
    equal(actions().map(call=>({action:call.action,id:call.id,value:call.value})),[{action:'acknowledge',id:'fixture_discord',value:'fixture_turn_00002'}],'Turn/channel identity mismatch');
  });
  await test('Running recovery controls stay disabled and cannot resolve',async()=>{
    await F.set([F.row('fixture_telegram',{running:true,deliveries:12,turns:2})]);F.manage();
    const buttons=[...modal().querySelectorAll('[data-recover]')];assert(buttons.length>0&&buttons.every(button=>button.disabled),'Running recovery action enabled');
    buttons.forEach(button=>button.click());equal(actions().length,0,'Running channel sent a recovery mutation');
  });
  await test('Closing a delayed login check cannot update the next dialog',async()=>{
    await F.set([F.row(),F.row('fixture_discord')]);F.manage();const gate=F.hold('check');
    try{click('[data-check]');await until(()=>gate.taken);close();F.manage(1);gate.resolve();await settle();
      equal(get('#channelTitle').textContent,'Project Discord','Stale check replaced a later dialog');
      assert(!modal().innerText.includes('Bot login verified'),'Stale login result leaked to the new dialog');
    }finally{gate.resolve();}
  });
  await test('A late failed action cannot surface in a reopened dialog',async()=>{
    await F.set([F.row(),F.row('fixture_discord')]);F.manage();const gate=F.hold('check');
    try{click('[data-check]');await until(()=>gate.taken);close();F.manage(1);gate.reject(Error('Old request failed'));await settle();
      equal(get('#channelTitle').textContent,'Project Discord','Old failure replaced the new dialog');
      assert(!modal().querySelector('[role="alert"]'),'Stale error leaked to the new dialog');
    }finally{gate.resolve();}
  });
  await test('A stale list response cannot overwrite a newer render',async()=>{
    await F.set([F.row()]);const gate=F.hold('list'),old=PhoenixChannels.render(F.host);
    try{await until(()=>gate.taken);F.mutate(rows=>rows[0].config.name='Newest channel name');await PhoenixChannels.render(F.host);gate.resolve();await old;
      assert(F.host.innerText.includes('Newest channel name'),'Old list overwrote current rows',F.host.innerText);
    }finally{gate.resolve();}
  });
  await test('Refresh keeps keyboard focus through success and failure',async()=>{
    await F.set([F.row()]);click('[data-channel-refresh]',F.host);await settle();
    equal(document.activeElement,F.host.querySelector('[data-channel-refresh]'),'Refresh lost focus after success');
    F.fail('list');click('[data-channel-refresh]',F.host);await settle();
    equal(document.activeElement,F.host.querySelector('[data-channel-refresh]'),'Refresh lost focus after failure');
  });
  await test('Delayed refresh does not steal focus from another control',async()=>{
    await F.set([F.row()]);const gate=F.hold('list');
    try{click('[data-channel-refresh]',F.host);await until(()=>gate.taken);const other=document.getElementById('fixture-theme');other.focus();gate.resolve();await settle();equal(document.activeElement,other,'Refresh stole a newer focus choice');}finally{gate.resolve();}
  });
  await test('Closing a pending resolution leaves the next channel dialog intact',async()=>{
    await F.set([F.row('fixture_telegram',{deliveries:3}),F.row('fixture_discord',{deliveries:3})]);F.manage();const gate=F.hold('received');
    try{click('[data-recover="received"]');await until(()=>gate.taken);close();F.manage(1);gate.resolve();await settle();
      equal(get('#channelTitle').textContent,'Project Discord','Old resolution replaced a later dialog');
      equal(F.data().map(row=>row.status.delivery.review_deliveries.length),[2,3],'Old resolution changed the wrong channel');
      equal(actions().length,1,'Closing the pending action duplicated it');
    }finally{gate.resolve();}
  });
  await test('Allowed IDs split on spaces and commas without numeric conversion',async()=>{
    const form=await addForm({users:'123 456, 18446744073709551615'});form.requestSubmit();await settle();
    equal(F.calls.find(call=>call.action==='save')?.config.allowed_user_ids,['123','456','18446744073709551615'],'Incorrect allowed ID serialization');
    assert(!modal(),'Valid save failed');
  });
  for(const [name,field,value,extra] of [
    ['Whitespace-only name','name','   ',{}],
    ['Malformed allowed user','users','123 nope',{}],
    ['Negative allowed user','users','-123',{}],
    ['Oversized allowed user','users','123456789012345678901',{}],
    ['Negative Discord channel','destination','-123',{platform:'discord'}],
    ['Oversized destination','destination','123456789012345678901',{}],
    ['Relative workspace','workspace','relative/folder',{}]
  ])await test(`${name} is blocked before invoking save`,async()=>{
    const form=await addForm({...extra,[field]:value});form.requestSubmit();await settle();
    equal(F.calls.filter(call=>call.action==='save').length,0,'Invalid form reached the API');
    equal(document.activeElement?.name,field,'Invalid field did not receive focus');
  });
  await test('Negative Telegram destination remains valid',async()=>{
    const form=await addForm({destination:'-100123456789'});form.requestSubmit();await settle();
    equal(F.calls.find(call=>call.action==='save')?.config.conversation_id,'-100123456789','Telegram destination changed');assert(!modal(),'Valid Telegram destination was rejected');
  });
  await test('An empty workspace in a collapsed disclosure opens and receives focus',async()=>{
    const form=await addForm({workspace:''});form.querySelector('details').open=false;form.querySelector('[type=submit]').focus();form.requestSubmit();await settle();
    assert(form.querySelector('details').open,'Invalid workspace remains hidden');equal(document.activeElement.name,'workspace','Hidden invalid workspace cannot receive focus');equal(actions().length,0,'Empty workspace reached the API');
  });
  await test('Missing bot token is blocked and focused before invocation',async()=>{
    await F.set([F.row('fixture_telegram',{saved:false})]);F.manage();click('[data-connect]');await settle();
    equal(actions().length,0,'Empty token reached the API');equal(document.activeElement?.name,'token','Token field did not receive focus');
  });
  await test('Successful login check clears a prior error and restores control focus',async()=>{
    await F.set([F.row()]);F.manage();F.fail('check');click('[data-check]');await settle();
    assert(modal().querySelector('[role="alert"]'),'Fixture failure is missing');click('[data-check]');await settle();
    assert(get('[data-login-result]').textContent.includes('verified'),'Successful check is missing');
    assert(!modal().querySelector('[role="alert"]'),'Old error remains after success');
    equal(document.activeElement,get('[data-check]'),'Action lost keyboard focus');
  });
  await test('Keyboard wraps inside the dialog and Escape returns to Manage',async()=>{
    await F.set([F.row()]);F.manage();const closeButton=get('.modal-close'),summary=[...modal().querySelectorAll('summary')].at(-1);
    closeButton.focus();closeButton.dispatchEvent(new KeyboardEvent('keydown',{key:'Tab',shiftKey:true,bubbles:true,cancelable:true}));equal(document.activeElement,summary,'Shift-Tab escaped');
    summary.dispatchEvent(new KeyboardEvent('keydown',{key:'Tab',bubbles:true,cancelable:true}));equal(document.activeElement,closeButton,'Tab escaped');
    const bubbles=F.escapeBubbles;closeButton.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true,cancelable:true}));await settle();
    assert(!modal(),'Escape did not close');equal(document.activeElement,F.host.querySelector('[data-channel-manage]'),'Escape lost opener focus');equal(F.escapeBubbles,bubbles,'Escape bubbled into outer settings');
  });
  await test('Closing nested Edit returns focus to the live Manage opener',async()=>{
    await F.set([F.row()]);F.manage();const details=get('[data-edit]').closest('details');details.open=true;click('[data-edit]');close();await settle();
    equal(document.activeElement,F.host.querySelector('[data-channel-manage]'),'Nested dialog lost focus to a removed element');
  });
  await test('Backdrop close restores the live opener',async()=>{
    await F.set([F.row()]);F.manage();document.getElementById('modalLayer').dispatchEvent(new PointerEvent('pointerdown',{bubbles:true}));await settle();
    assert(!modal(),'Backdrop did not close');equal(document.activeElement,F.host.querySelector('[data-channel-manage]'),'Backdrop lost opener focus');
  });
  await test('Page changes keep navigation and Close reachable at scrolled positions',async()=>{
    await F.set([F.row('fixture_telegram',{deliveries:21})]);F.manage();const node=modal();node.scrollTop=node.scrollHeight;
    const next=get('[data-recovery-next]');next.focus({preventScroll:true});next.click();await settle();
    assert(node.contains(document.activeElement),'Page change lost focus outside the modal');
    const box=node.getBoundingClientRect(),nav=get('.channel-recovery-nav').getBoundingClientRect(),header=get('.modal-header').getBoundingClientRect();
    assert(nav.top>=box.top&&nav.bottom<=box.bottom,'Page navigation is out of view',{box:box.toJSON(),nav:nav.toJSON()});
    assert(header.top>=box.top-1&&header.bottom<=box.bottom,'Close header is out of view');
    assert(node.scrollWidth<=node.clientWidth+1,'Recovery overflows horizontally');return {scrollHeight:node.scrollHeight,clientHeight:node.clientHeight,scrollTop:node.scrollTop};
  });
  await test('Refresh failure after resolution cannot resend the resolved action',async()=>{
    await F.set([F.row('fixture_telegram',{deliveries:12})]);F.manage();F.fail('list');click('[data-recover="received"]');await settle();
    equal(actions().length,1,'Resolution did not complete once');equal(F.data()[0].status.delivery.review_deliveries.length,11,'Resolution did not persist');
    assert(modal(),'Refresh failure lost recovery context');assert(!modal().querySelector('[data-recover]:not([disabled])'),'Stale recovery controls remain actionable');
    click('[data-recovery-refresh]');await until(()=>modal()?.querySelector('[data-recovery-range]')?.textContent.includes('11'));
    equal(actions().length,1,'Refreshing repeated the mutation');
  });
  await test('No uncaught errors in the real component fixture',async()=>equal(F.errors,[],'Uncaught browser errors'));
  const result={kind:'synthetic API, production channel component in existing Chromium',timestamp:new Date().toISOString(),userAgent:navigator.userAgent,viewport:{width:innerWidth,height:innerHeight},passed:results.filter(row=>row.passed).length,failed:results.filter(row=>!row.passed).length,results};
  window.channelCheckResult=result;
  return result;
};
