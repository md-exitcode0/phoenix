'use strict';
const assert=require('node:assert/strict');
const {validateGateway}=require('../review-shell/launcher/backend-current.cjs');
const context={agents:[{lifecycle:'active',kind:'responsibility_owner',agent_id:'frontend',canonical_session_id:'agent-frontend'}],groups:[{lifecycle:'active',group_id:'room',canonical_session_id:'group-room'}],asks:[{ask_id:'ask-current',status:'pending',approval:{effect:'filesystem_write'}}]};
let count=0;
for(const owner of [{kind:'agent',id:'frontend'},{kind:'group',id:'room'}]){
 const body={ask_id:'ask-current',session_id:owner.kind==='agent'?'agent-frontend':'group-room',owner};
 for(const kind of ['DismissAsk','AnswerAsk']){
  const v={...body,...(kind==='AnswerAsk'?{answer:'My explicit answer'}:{})};
  assert.equal(validateGateway({[kind]:v},context),kind);count++;
  for(const bad of [{...v,owner:{...owner,id:'foreign'}},{...v,session_id:'wrong-thread'},{...v,ask_id:'old-ask'},{...v,ask_id:'../bad'},{...v,extra:'unknown'},{...v,owner:{...owner,extra:true}},...(kind==='AnswerAsk'?[{...v,answer:''},{...v,answer:123}]:[{...v,answer:'Approve'}])]){assert.throws(()=>validateGateway({[kind]:bad},context));count++;}
  for(const asks of [[],[{ask_id:'ask-current',status:'answered'}],[{ask_id:'ask-current',status:'dismissed'}]]){assert.throws(()=>validateGateway({[kind]:v},{...context,asks}));count++;}
 }
}
for(const request of [{Vault:{action:'unlock_with_password',master_password:'not-permitted'}},{Settings:{action:'set',key:'auth.provider'}},{Turn:{session_id:'agent-frontend',user_request:'start unrelated work'}}]){assert.throws(()=>validateGateway(request,context));count++;}
console.log(JSON.stringify({passed:count,scope:'Actual exported review authorization; owner/current-ask checks, dismissal separate from answers, protected routes remain protected'}));
