import assert from 'node:assert/strict';
import {mkdir,readFile,stat} from 'node:fs/promises';
import {join} from 'node:path';
import {withAcceptanceGateway} from './lib/acceptance-gateway.mjs';
const [output]=process.argv.slice(2);
if(!output?.startsWith('/'))throw Error('NEW absolute output path required');
await mkdir(output,{mode:0o700});
await assert.rejects(withAcceptanceGateway({binary:'/usr/bin/false',output,workspace:output,authSource:'codex',
  setup:async()=>async()=>{throw Error('Injected resource teardown failure');}},()=>{throw Error('Unreachable test body');}),
  /Owned gateway exited before readiness/);
const receipt=JSON.parse(await readFile(join(output,'execution.json')));
assert.equal(receipt.credentialRemoved,true);
assert.equal(receipt.cleanupFailures.length,1);
assert.equal(receipt.cleanupFailures[0].label,'test resource shutdown');
assert.ok(receipt.error.includes('Owned gateway exited before readiness'));
await assert.rejects(stat(join(receipt.home,'auth-profiles.json')),error=>error.code==='ENOENT');
console.log('PASS: failed gateway and failed resource teardown preserve the primary error, remove credentials and publish cleanup evidence; no model call');
