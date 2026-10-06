'use strict';
const assert=require('node:assert/strict');
const routing=require('../review-shell/launcher/browser-routing.cjs');
const context={agents:[{agent_id:'frontend',browser_profile_id:'agent-frontend',lifecycle:'active',kind:'responsibility_owner'},{agent_id:'school_coach',browser_profile_id:'agent-school_coach',lifecycle:'active',kind:'responsibility_owner'},{agent_id:'archived',browser_profile_id:'agent-archived',lifecycle:'archived',kind:'responsibility_owner'}]};
let passed=0;
for(const instance of ['agent-phoenix','agent-frontend','agent-school_coach']){
 for(const action of ['open','status','close']){assert.equal(routing.validate({BrowserSurface:{instance,action}},context).body.instance,instance);passed++;}
 for(const action of ['navigate','new_tab']){assert.equal(routing.validate({BrowserInteract:{instance,browser_action:{action,url:'https://example.invalid/path'}}},context).body.instance,instance);passed++;}
}
for(const instance of ['agent-unknown','agent-archived','agent-frontend-other','frontend',null]){assert.throws(()=>routing.validate({BrowserSurface:{instance,action:'open'}},context));passed++;}
for(const url of ['file:///private','javascript:alert(1)','data:text/html,hello','ftp://example.invalid']){assert.throws(()=>routing.validate({BrowserInteract:{instance:'agent-frontend',browser_action:{action:'navigate',url}}},context));passed++;}
for(const action of ['evaluate','cookies','signin','click']){assert.throws(()=>routing.validate({BrowserInteract:{instance:'agent-frontend',browser_action:{action}}},context));passed++;}
assert.equal(routing.validate({Turn:{}},context),null);passed++;
for(const action of ['click','type','send_keys','scroll']){
 assert.equal(routing.validate({BrowserInteract:{instance:'agent-frontend',browser_action:{action}}},{...context,currentBrowserInteractive:true}).action,action);passed++;
}
for(const action of ['evaluate','cookies','signin']){assert.throws(()=>routing.validate({BrowserInteract:{instance:'agent-frontend',browser_action:{action}}},{...context,currentBrowserInteractive:true}));passed++;}
for(const [width,height] of [[0,900],[700,0],[4096,900],[700,2000],[640.5,900]]){assert.throws(()=>routing.validate({BrowserInteract:{instance:'agent-frontend',browser_action:{action:'resize',width,height}}},context));passed++;}
assert.throws(()=>routing.validate({BrowserSurface:{instance:'agent-phoenix',action:'open'}},{agents:[],currentBrowserInteractive:true}));passed++;
console.log(JSON.stringify({passed,scope:'Actual browser route validation; no native browser/profile/network/model calls.'}));
