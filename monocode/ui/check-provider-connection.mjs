import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
const source=readFileSync(new URL('./provider-setup.js',import.meta.url),'utf8').replace('Object.freeze({ open, manage })','Object.freeze({ open, manage, connect, state, connectionFailureMessage })');
function harness(method, overrides={}) {
  const calls=[],messages=[];
  let closed=0,connected=0;
  const status={textContent:''};
  const form={isConnected:true,querySelectorAll(){return [];},querySelector(){return null;},append(){}};
  const nodes={providerSetupForm:form,providerSetupName:{value:'Personal Plus'},providerSetupProfile:{value:'do-not-use-as-id',dataset:{value:'openai-codex:default'}},providerSetupSecret:{value:'test-key'},providerSetupSubmit:{},providerSetupStatus:{classList:{toggle(){}},querySelector(){return status;}}};
  const ui={TAURI:true,escapeHtml:String,closeModal(){closed++;form.isConnected=false;},toast(message){messages.push(message);},async invoke(command,args){calls.push({command,args});if(overrides[command])return overrides[command](args);if(command==='auth_probe')throw Error('429 usage_limit_reached');}};
  const context={window:{PhoenixUI:ui},document:{getElementById(id){return nodes[id]||null;},createElement(){return {append(){}};}},console};
  vm.runInNewContext(source,context);
  const setup=context.window.PhoenixProviderSetup;
  Object.assign(setup.state,{provider:'openai-codex',method,label:'Personal Plus',catalog:{providers:[{id:'openai-codex',name:'OpenAI Codex',profiles:[{id:'openai-codex:default',method:'oauth'}]}]},onConnected(){connected++;}});
  return {setup,nodes,calls,messages,status,closed:()=>closed,connected:()=>connected,submit:()=>setup.connect({preventDefault(){}})};
}
for(const method of ['oauth','device_code','api','existing']){
  const h=harness(method);await h.submit();
  assert.equal(h.closed(),1,method);assert.equal(h.connected(),1,method);
  assert.equal(h.calls.some(c=>c.command==='auth_probe'),false,method);
  assert.equal(h.messages.at(-1),'OpenAI Codex is connected.');
  const rename=h.calls.find(c=>c.command==='auth_rename');
  if(method==='existing')assert.equal(rename,undefined);
  else {assert.equal(rename.args.label,'Personal Plus');assert.equal(rename.args.profileId,'openai-codex:2');}
}
// Retrying metadata after successful OAuth/API storage must not rotate or replace credentials again.
for (const method of ['oauth','device_code','api']) {
  let attempts=0;
  const h=harness(method,{auth_rename(){if(++attempts===1)throw Error('store busy');}});
  h.setup.state.initialize=true;
  await h.submit();assert.equal(h.closed(),0);assert.equal(h.setup.state.savedProfileId,'openai-codex:2');
  assert.equal(h.nodes.providerSetupSubmit.textContent,'Finish saving');
  await h.submit();assert.equal(h.closed(),1);
  assert.equal(h.calls.filter(c=>['oauth_login','auth_set_key'].includes(c.command)).length,1);
  assert.equal(h.calls.find(c=>c.command==='provider_initialize_config').args.authMethod,method==='device_code'?'token':method);
}
// Reconnect names the same identity, never allocates a fresh slot.
{const h=harness('oauth');h.setup.state.replaceProfileId='openai-codex:default';await h.submit();assert.equal(h.calls[0].args.profileId,'openai-codex:default');}
// An old sign-in must not close or reset a newer modal, nor use its account name or callback.
{
  let finish;const h=harness('oauth',{oauth_login(){return new Promise(resolve=>finish=resolve);}});
  const first=h.submit();h.setup.state.session++;h.setup.state.busy=true;h.setup.state.label='Work';h.setup.state.onConnected=()=>{throw Error('wrong callback');};
  finish();await first;assert.equal(h.closed(),0);assert.equal(h.connected(),1);assert.equal(h.setup.state.busy,true);assert.equal(h.calls.at(-1).args.label,'Personal Plus');
}
// Invalid names cause no credential mutation; name text is independent of internal keys.
for(const label of ['', 'x'.repeat(81), 'bad\nname']){const h=harness('oauth');h.nodes.providerSetupName.value=label;h.setup.state.label=label;await h.submit();assert.equal(h.calls.length,0);}
console.log('PASS: names, stable IDs, reconnect, quota-independent setup, partial-save retries, input validation, and stale sign-in isolation.');

{
  const {setup}=harness('oauth');
  assert.match(setup.connectionFailureMessage('429 usage_limit_reached'),/sign-in is still saved/);
  assert.match(setup.connectionFailureMessage('401 unauthorized'),/Sign-in needs attention/);
  assert.match(setup.connectionFailureMessage('OAuth token refresh returned HTTP 400: the provider rejected the saved login; sign in again'),/Sign-in needs attention/);
  assert.match(setup.connectionFailureMessage('OAuth token refresh returned HTTP 503: automatic sign-in renewal failed'),/Try again shortly/);
  assert.match(setup.connectionFailureMessage('503 unavailable'),/Try again shortly/);
  assert.doesNotMatch(setup.connectionFailureMessage('429 quota'),/signing in again/);
}
console.log('PASS: quota, expired login, and provider outage have distinct short messages.');
