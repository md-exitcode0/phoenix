'use strict';
const assert=require('node:assert/strict'),policy=require('../review-shell/launcher/presentation-write-policy.cjs');
const directory={agents:[{agent_id:'frontend',canonical_session_id:'company-frontend',lifecycle:'active'}],groups:[{group_id:'team',canonical_session_id:'group-team',lifecycle:'active'}]};
const base={sessionId:'company-frontend',owner:{kind:'agent',id:'frontend'},updates:{'conversation:display:v2':JSON.stringify({version:2,rows:[{value:{text:'fixture only'}}]})}};
let passed=0;
function accept(args){assert.equal(policy.validate(args,directory),true);passed++;}
function deny(args){assert.throws(()=>policy.validate(args,directory),/Invalid conversation presentation/);passed++;}
accept(base);accept({...base,updates:{'answer:abc123':JSON.stringify({created_at:null,elapsed_ms:1500})}});
accept({...base,sessionId:'group-team',owner:{kind:'group',id:'team'}});
for(const args of [
 {...base,sessionId:'company-other'}, {...base,owner:{kind:'agent',id:'other'}}, {...base,owner:{kind:'group',id:'frontend'}},
 {...base,owner:{...base.owner,extra:true}}, {...base,permissions:'full_access'}, {...base,updates:{'config.toml':'{}'}},
 {...base,updates:{'conversation:display:v2':'{'}}, {...base,updates:{'conversation:display:v2':JSON.stringify({version:1,rows:[]})}},
 {...base,updates:{'conversation:display:v2':JSON.stringify({version:2,rows:{}})}}, {...base,updates:{'conversation:display:v2':null}},
 {...base,updates:{'answer:abc':JSON.stringify({created_at:null,elapsed_ms:-1})}},
 {...base,updates:{'answer:abc':JSON.stringify({created_at:null,elapsed_ms:0,command:'bash'})}},
 {...base,updates:{}}, {...base,updates:{'conversation:display:v2':'x'.repeat(7*1024*1024+1)}},
])deny(args);
assert.throws(()=>policy.validate(base,{agents:[{...directory.agents[0],lifecycle:'archived'}]}));passed++;
console.log(JSON.stringify({passed,scope:'Actual presentation-write validator; no live feed writes or model calls.'}));
