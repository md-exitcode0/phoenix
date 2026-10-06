/* Actual production component; synthetic bounded server API, explicitly separate from legacy-array checks. */
"use strict";
window.runPagedChannelChecks=async function(){
  const F=ChannelFixture,results=[],modal=()=>document.querySelector('.channel-dialog');
  const get=(selector,root=modal())=>{const node=root?.querySelector(selector);if(!node)throw Error(`Missing ${selector}`);return node;};
  const assert=(ok,message)=>{if(!ok)throw Error(message);};
  const until=async(predicate)=>{for(let i=0;i<200;i++){if(predicate())return;await new Promise(resolve=>setTimeout(resolve,5));}throw Error('Expected page state was not reached');};
  const ready=()=>until(()=>modal()&&!modal().dataset.busy&&modal().querySelector('[data-recover]'));
  const click=(selector)=>{const button=get(selector);button.focus();button.click();return button;};
  const codes=()=>[...modal().querySelectorAll('.channel-recovery code')].map(node=>node.textContent);
  const mutations=()=>F.calls.filter(call=>['received','retry','acknowledge'].includes(call.action));
  const page=async(value)=>{const input=get('[data-recovery-page]');input.value=String(value);input.form.requestSubmit();await ready();};
  const test=async(name,fn)=>{try{const evidence=await fn();results.push({name,passed:true,...(evidence?{evidence}:{})});}catch(error){results.push({name,passed:false,error:error.message});}};
  const setup=async(deliveries=27,turns=3)=>{F.setPaged(true);await F.set([F.row('fixture_telegram',{deliveries,turns})]);F.manage();await ready();};
  try{
    await test('Bounded API transfers only summary and ten requested IDs from 160,000 replies',async()=>{
      await setup(160000,999);
      assert(F.responses[0].action==='list'&&!F.responses[0].hasArrays,'List transferred full recovery arrays');
      assert(F.responses.every(r=>r.bytes<16*1024),'Summary or excerpt page exceeded small fixture budget');
      assert(codes().length===10,'Page rendered more than ten IDs');
      assert(get('[data-recovery-kind]').textContent.includes('160,000'),'Complete reply count missing');
      await page(16000);assert(codes().at(-1)==='fixture_delivery_160000','Last reply lost');
      const select=get('[data-recovery-kind]');select.value='turns';select.dispatchEvent(new Event('change',{bubbles:true}));await ready();await page(100);
      assert(codes().at(-1)==='fixture_turn_00999','Last interrupted turn lost');
      assert(mutations().length===0,'Page reads mutated state');
      return {responses:F.responses.slice(),reviewRequests:F.calls.filter(call=>call.action==='review').map(({kind,offset,limit,snapshot})=>({kind,offset,limit,snapshot}))};
    });
    await test('Final-page resolution is exact, clamped and never double submitted',async()=>{
      await setup(11,0);await page(2);const target=get('[data-recover="received"]'),gate=F.hold('received');
      try{target.click();target.click();await until(()=>gate.taken);assert(mutations().length===1,'Duplicate recovery action');
        assert(mutations()[0].value==='fixture_delivery_00011'&&/^[a-f0-9]{64}$/.test(mutations()[0].snapshot),'Exact ID/snapshot missing');
        gate.resolve();await until(()=>!modal()?.dataset.busy&&get('[data-recovery-range]').textContent.includes('of 10'));
        assert(get('[data-recovery-page]').value==='1'&&codes().length===10,'Last-page clamp failed');
        assert(F.data()[0].status.delivery.review_deliveries.length===10,'Resolution removed the wrong number of records');
        return {receipt:mutations()[0],remaining:10};
      }finally{gate.resolve();}
    });
    await test('Reorder invalidates page snapshot and explicit refresh loads current IDs',async()=>{
      await setup();const firstSnapshot=F.calls.find(c=>c.action==='review').snapshot;
      F.mutate(rows=>rows[0].status.delivery.review_deliveries.reverse());click('[data-recovery-next]');
      await until(()=>!modal().dataset.busy&&get('[data-recovery-range]').textContent.includes('refreshing'));
      assert(codes().length===0&&!modal().querySelector('[data-recover]'),'Stale actions survived failed page read');
      const before=F.calls.length;await new Promise(resolve=>setTimeout(resolve,30));assert(F.calls.length===before,'Failed page automatically retried');
      click('[data-recovery-refresh]');await ready();assert(codes()[0]==='fixture_delivery_00027','Fresh reorder was not read');
      assert(firstSnapshot===null&&F.calls.filter(c=>c.action==='review').at(-1).snapshot===null,'Refresh retained old revision');
    });
    await test('Changed state rejects an old exact action without altering records',async()=>{
      await setup();F.mutate(rows=>rows[0].status.delivery.review_deliveries[0].state='rejected');click('[data-recover="received"]');
      await until(()=>!modal().dataset.busy&&modal().querySelector('[role="alert"]'));
      assert(F.data()[0].status.delivery.review_deliveries.length===27,'Stale mutation altered records');
      assert(mutations().length===1,'Mutation was replayed');click('[data-recovery-refresh]');await ready();
      assert(get('.channel-recovery-state').textContent.startsWith('Delivery rejected'),'Refresh did not show changed state');
    });
    await test('Closing a delayed page cannot replace the next channel dialog',async()=>{
      F.setPaged(true);await F.set([F.row('fixture_telegram',{deliveries:20}),F.row('fixture_discord',{deliveries:12})]);
      const gate=F.hold('review');try{F.manage();await until(()=>gate.taken);click('.modal-close');F.manage(1);await ready();gate.resolve();
        await new Promise(resolve=>setTimeout(resolve,20));assert(get('#channelTitle').textContent==='Project Discord','Old page replaced new channel');
        assert(get('[data-recovery-range]').textContent.includes('of 12'),'Old total replaced new total');
      }finally{gate.resolve();}
    });
    for(const [name,transform] of [
      ['foreign channel',r=>({...r,id:'another_channel'})],
      ['wrong kind',r=>({...r,kind:'turns'})],
      ['wrong offset',r=>({...r,offset:10})],
      ['inconsistent total',r=>({...r,total:999})],
      ['duplicate IDs',r=>({...r,items:r.items.map(()=>r.items[0])})],
      ['oversized ID',r=>({...r,items:r.items.map((item,i)=>i?item:{...item,id:'x'.repeat(129)})})],
      ['oversized excerpt',r=>({...r,items:r.items.map((item,i)=>i?item:{...item,preview:{kind:'text',text:'x'.repeat(201),truncated:true}})})],
      ['invalid excerpt kind',r=>({...r,items:r.items.map((item,i)=>i?item:{...item,preview:{kind:'private_transcript',text:'No',truncated:false}})})],
      ['malformed snapshot',r=>({...r,snapshot:'not-a-snapshot'})],
      ['oversized page',r=>({...r,items:[...r.items,...r.items]})]
    ])await test(`Malformed ${name} response never exposes recovery actions`,async()=>{
      F.setPaged(true);await F.set([F.row('fixture_telegram',{deliveries:27})]);F.transformReview(transform);F.manage();
      await until(()=>!modal().dataset.busy&&modal().querySelector('[role="alert"]'));
      assert(codes().length===0&&!modal().querySelector('[data-recover]'),'Malformed response exposed an action');
      assert(mutations().length===0,'Malformed response caused a mutation');
    });
    await test('Refresh failure after success never retries the recovery mutation',async()=>{
      await setup(12,0);F.fail('list');click('[data-recover="received"]');
      await until(()=>!modal().dataset.busy&&modal().querySelector('[data-recovery-refresh]')&&!modal().querySelector('[data-recover]'));
      assert(mutations().length===1&&F.data()[0].status.delivery.review_deliveries.length===11,'Exact recovery did not persist once');
      click('[data-recovery-refresh]');await ready();assert(mutations().length===1,'Refresh replayed recovery');
      assert(get('[data-recovery-range]').textContent.includes('of 11'),'Remaining count missing');
    });
    await test('Paged navigation restores keyboard focus and disables its final Next',async()=>{
      await setup(20,0);const next=click('[data-recovery-next]');await ready();
      assert(next.disabled,'Final Next remains enabled after async control restoration');
      assert(modal().contains(document.activeElement),'Async page change lost focus to the document');
      assert(!get('[data-recovery-prev]').disabled,'Previous not restored');
      click('[data-recovery-prev]');await ready();assert(get('[data-recovery-prev]').disabled&&!get('[data-recovery-next]').disabled,'First-page control state incorrect');
    });
    await test('Recovery fits the viewport and shows one primary action with real excerpts',async()=>{
      await setup(27,3);await new Promise(resolve=>setTimeout(resolve,250));
      const rect=modal().getBoundingClientRect();
      assert(rect.left>=0&&rect.right<=innerWidth&&rect.top>=0&&rect.bottom<=innerHeight,'Recovery modal clips outside viewport');
      assert(modal().scrollWidth<=modal().clientWidth+1,'Recovery overflows horizontally');
      const visible=[...modal().querySelectorAll('[data-recover].primary')].filter(button=>button.getClientRects().length&&!button.closest('details:not([open])'));
      assert(visible.length===1,'More than one primary recovery action is exposed');
      const original=F.data()[0].status.delivery.review_deliveries[0];
      assert(get('.channel-recovery-copy strong').textContent===original.preview.text,'Displayed excerpt differs from stored fixture content');
      assert(get('.channel-reference code').closest('details:not([open])'),'Opaque reference is exposed before disclosure');
      const second=modal().querySelectorAll('[data-recovery-record]')[1];second.open=true;
      await new Promise(resolve=>setTimeout(resolve,20));
      assert(modal().querySelectorAll('[data-recovery-record][open]').length===1,'Selecting a reply left multiple recovery actions expanded');
      return {rect:rect.toJSON(),viewport:{width:innerWidth,height:innerHeight},visiblePrimaryActions:visible.length};
    });
    await test('Paged API has no browser errors',async()=>assert(F.errors.length===0,JSON.stringify(F.errors)));
  }finally{F.setPaged(false);}
  const result={kind:'Production channel component, synthetic bounded page API in actual browser',timestamp:new Date().toISOString(),viewport:{width:innerWidth,height:innerHeight},passed:results.filter(r=>r.passed).length,failed:results.filter(r=>!r.passed).length,results};
  window.pagedChannelCheckResult=result;return result;
};
