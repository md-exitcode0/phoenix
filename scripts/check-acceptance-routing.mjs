// Exercise the real isolated setup/cleanup with synthetic credentials and a
// deliberately exiting executable. No browser, provider request or real login.
import assert from 'node:assert/strict';
import {mkdtemp,mkdir,readFile,writeFile,stat} from 'node:fs/promises';
import {join} from 'node:path';
import {withAcceptanceGateway} from './lib/acceptance-gateway.mjs';

const [output]=process.argv.slice(2);
if(!output?.startsWith('/'))throw Error('NEW absolute evidence directory required');
await mkdir(output,{mode:0o700});
const source=await mkdtemp(join(output,'synthetic-source-'));
const expires=Date.now()+3600000;
const credential=(access)=>({type:'oauth',provider:'openai-codex',access,
  refresh:'SYNTHETIC-REFRESH-DO-NOT-COPY',expires,private_extra:'DO-NOT-COPY'});
const auth={version:1,profiles:{first:credential('SYNTHETIC-FIRST'),second:credential('SYNTHETIC-SECOND')},
  labels:{first:'First account',second:'Second account'},state:{order:{'provider:openai-codex':['second','first']}}};
const authBytes=JSON.stringify(auth);
await writeFile(join(source,'auth-profiles.json'),authBytes,{mode:0o600});
await writeFile(join(source,'config.toml'),'[profile.llm]\nprovider="openai-codex"\n[profile.llm.auth]\nsource="profile"\nprofile="first"\n',{mode:0o600});
const previousHome=process.env.PHOENIX_HOME;
const results=[];
try {
  process.env.PHOENIX_HOME=source;
  for(const test of [
    {name:'configured',options:{},ids:['first']},
    {name:'pool',options:{accountPool:true},ids:['second','first']},
    {name:'explicit',options:{authProfile:'second'},ids:['second']},
  ]) {
    const path=join(output,test.name);await mkdir(path);
    let setupChecked=false;
    await assert.rejects(withAcceptanceGateway({binary:'/usr/bin/false',output:path,workspace:path,...test.options,
      setup:async({home})=>{
        const copied=JSON.parse(await readFile(join(home,'auth-profiles.json')));
        const config=await readFile(join(home,'config.toml'),'utf8');
        const entries=Object.values(copied.profiles);
        assert.equal(entries.length,test.ids.length);
        assert.deepEqual(entries.map(row=>row.token),test.ids.map(id=>auth.profiles[id].access));
        assert.ok(entries.every(row=>row.type==='token'&&row.provider==='openai-codex'));
        assert.ok(!/refresh|private_extra|DO-NOT-COPY/.test(JSON.stringify(copied)));
        assert.match(config,/model="gpt-5\.6-sol"/);
        assert.match(config,/reasoning_effort="medium"/);
        setupChecked=true;
        return async()=>{throw Error('Synthetic cleanup failure');};
      }},()=>{throw Error('No model task may run');}),/Owned gateway exited before readiness/);
    assert.ok(setupChecked);
    const receipt=JSON.parse(await readFile(join(path,'execution.json')));
    const selection=JSON.parse(await readFile(join(path,'account-selection.json')));
    assert.equal(selection.sourceHome,source);
    assert.equal(selection.authSource,'phoenix');
    assert.deepEqual(selection.selectedAccounts.map(row=>row.sourceProfileId),test.ids);
    assert.deepEqual(selection.selectedAccounts,receipt.selectedAccounts);
    assert.ok(selection.selectedAccounts.every(row=>row.authenticated===false&&row.quotaChecked===false));
    assert.ok(!/SYNTHETIC-FIRST|SYNTHETIC-SECOND|DO-NOT-COPY/.test(JSON.stringify(selection)+JSON.stringify(receipt)));
    assert.equal(receipt.credentialRemoved,true);
    assert.equal(receipt.cleanupFailures.length,1);
    assert.match(receipt.error,/Owned gateway exited/);
    await assert.rejects(stat(join(receipt.home,'auth-profiles.json')),error=>error.code==='ENOENT');
    results.push({name:test.name,selectedProfileIds:test.ids,setupChecked,credentialRemoved:true,
      primaryErrorPreserved:true,publicProvenanceOnly:true});
  }
  const missing=join(output,'missing');await mkdir(missing);
  await assert.rejects(withAcceptanceGateway({binary:'/usr/bin/false',output:missing,workspace:missing,
    authProfile:'not-present'},()=>{throw Error('No model task may run');}),/not a connected Codex profile/);
  const rejected=JSON.parse(await readFile(join(missing,'execution.json')));
  assert.equal(rejected.gateway,null);
  assert.equal(rejected.credentialRemoved,true);
  assert.deepEqual(rejected.selectedAccounts,[]);
  assert.equal(await readFile(join(source,'auth-profiles.json'),'utf8'),authBytes);
  await writeFile(join(output,'result.json'),JSON.stringify({passed:true,results,missingProfileFailsBeforeStart:true,
    sourceUnchanged:true,providerCalls:0,browserStarts:0,credentials:'synthetic only'},null,2));
  console.log('PASS: real configured/pool/explicit setup, selected-source home, public profile identities, no secret export, missing-profile rejection and failure cleanup. Zero provider calls.');
} finally {
  if(previousHome===undefined)delete process.env.PHOENIX_HOME;
  else process.env.PHOENIX_HOME=previousHome;
}
