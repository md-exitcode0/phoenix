import {bananaQualityBrief} from './lib/banana-quality-brief.mjs';
// A real Sol-medium task using Phoenix computer tools. No geometry is supplied
// by the harness, and no Python/console/MCP creation can qualify as GUI success.
import {mkdir, writeFile, open, readFile, copyFile, stat, cp, readdir, lstat, realpath} from 'node:fs/promises';
import {join, resolve, relative} from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {createReadStream} from 'node:fs';
import {messages, once, withAcceptanceGateway} from './lib/acceptance-gateway.mjs';
import {collectDesktopEvidence} from './collect-desktop-evidence.mjs';
import {auditBlenderTools, currentBlenderTurn, plainNativeContinuation, nativeCompletionChecks} from './lib/blender-tool-audit.mjs';
import {acceptanceAuthOptions} from './lib/acceptance-account.mjs';

const [binaryArg, blenderArg, outputArg, ...options] = process.argv.slice(2);
const legacyGuided = options.includes('--legacy-guided');
const prepareOnly = options.includes('--prepare-only');
const resumeOption = options.find(option => option.startsWith('--resume-from='));
if(options.filter(option => option.startsWith('--resume-from=')).length > 1)throw Error('Choose one resume source');
if(options.filter(option=>option==='--prepare-only').length>1)throw Error('Duplicate preparation option');
const auth = acceptanceAuthOptions(options.filter(option => option !== '--legacy-guided' && option !== '--prepare-only' && option !== resumeOption));
const prior = resumeOption?.slice('--resume-from='.length);
if(prior && !prior.startsWith('/'))throw Error('Resume evidence path must be absolute');
let recovery;
async function hashFile(path){
  const hash=createHash('sha256');for await(const bytes of createReadStream(path))hash.update(bytes);
  return hash.digest('hex');
}
if(prior){
  const receipt = JSON.parse(await readFile(join(prior,'execution.json'),'utf8'));
  if(receipt.gateway?.code !== 0 || !receipt.credentialRemoved || !receipt.home?.startsWith('/tmp/phoenix-ui-acceptance-'))
    throw Error('Resume requires the prior owned gateway to have a clean terminal receipt');
  let source=join(prior,'recovery','banana-draft.blend'),continuation,sourceWorkspace;
  if(!legacyGuided){
    const setup=JSON.parse(await readFile(join(prior,'setup.json'),'utf8'));
    const metrics=JSON.parse(await readFile(join(prior,'metrics.json'),'utf8'));
    sourceWorkspace=await realpath(setup.workspace||join(prior,'work'));
    const evidenceRoot=await realpath(fileURLToPath(new URL('../artifacts/',import.meta.url)));
    if(relative(evidenceRoot,sourceWorkspace).startsWith('..')||sourceWorkspace===evidenceRoot)
      throw Error('Native continuation workspace is outside this project evidence root');
    const files=await readdir(sourceWorkspace,{withFileTypes:true});
    continuation=plainNativeContinuation({execution:receipt,setup,metrics},files.filter(file=>file.isFile()).map(file=>file.name));
    if((await lstat(join(prior,'durable-state'))).isSymbolicLink()
      ||await realpath(receipt.durableState)!==await realpath(join(prior,'durable-state')))
      throw Error('Native continuation requires the exact stopped evidence snapshot');
    source=join(sourceWorkspace,continuation.sceneName);
    if(!(await stat(source)).size)throw Error('Saved native draft is empty');
  }
  const hash = await hashFile(source);
  recovery = {home:continuation?receipt.durableState:receipt.home,source,hash,continuation,sourceWorkspace};
}
if (![binaryArg, blenderArg, outputArg].every(p => p?.startsWith('/')))
  throw Error('Absolute Phoenix binary, Blender executable, and NEW evidence directory required');
const binary = resolve(binaryArg), blender = resolve(blenderArg), output = resolve(outputArg);
await mkdir(output, {mode:0o700});
const workspace = recovery?.sourceWorkspace || join(output, 'work');
if(recovery?.continuation){
  await cp(workspace,join(output,'before'),{recursive:true,errorOnExist:true,force:false,
    filter:path=>!relative(workspace,path).split('/').some(part=>['.git','node_modules','.cache'].includes(part))});
  recovery.checkpoint=join(output,'before',recovery.continuation.sceneName);
  if(await hashFile(recovery.checkpoint)!==recovery.hash)
    throw Error('Native draft changed while preserving its checkpoint');
}else await mkdir(workspace);
if(!legacyGuided&&!recovery){
  const reference = new URL('../artifacts/banana-reference/banana-reference.jpg',import.meta.url);
  await copyFile(reference,join(workspace,'banana-reference.jpg'));
}
if(recovery){
  if(!recovery.continuation)await copyFile(recovery.source,join(workspace,'banana.blend'));
  await writeFile(join(output,'recovery-source.json'),JSON.stringify({prior,source:recovery.source,sha256:recovery.hash},null,2),{flag:'wx'});
}
const binaryHash = await hashFile(binary);
await writeFile(join(output, 'binary.json'), JSON.stringify({binary, sha256:binaryHash,
  model:'gpt-5.6-sol', reasoning_effort:'medium'}, null, 2), {flag:'wx'});
const prompt = recovery?.continuation?.prompt || (legacyGuided ? [
    bananaQualityBrief,
  'Create a polished, recognizable banana in Blender using the actual application interface.',
  recovery
    ? 'Continue the saved task. Open Phoenix Blender Acceptance; it loads the protected draft from the previous run into your own desktop. The previous final file was empty, but its backup retains one unfinished Bezier curve. Inspect that draft and repair or rebuild its defective parts through the UI. Reuse the existing durable goal and reconcile its expired lease before claiming the unfinished node. The previous test cancellation is historical; this turn has a fresh test deadline. Preserve the full quality bar. Edit only the copied scene at the paths below; prior-run paths are evidence, not destinations. Repeatedly reopening the same empty file failed last time: inspect the real file dialog and confirm the intended draft instead of repeating blind coordinates.'
    : 'Open the installed test application named Phoenix Blender Acceptance. It starts an empty factory scene in your own desktop.',
  'The banana should have a clearly curved, tapered body, a short stem, a yellow surface, smooth shading, and a pleasing uncluttered presentation.',
  `Save the editable scene as ${join(workspace, 'banana.blend')}. Save a clean preview image as ${join(workspace, 'banana.png')} through Blender if possible.`,
  'Aim to finish in approximately ten minutes. Use your judgment about modeling and presentation; the result must be recognizable and complete.',
  'Work only through computer_* tools. You may use todo_write, work for the private durable goal, image_analyze, recall, and final_answer when needed.',
  'Do not use Python, bpy, scripts, a console, a terminal, command execution, APIs, MCP, browser automation, generated/imported assets, or delegation to create or repair the scene.',
  'Use screenshots to see the interface and verify changes. Batch familiar, stable UI actions when useful. Do not claim an action succeeded merely because a tool returned successfully.',
  'Only work in this disposable Blender instance and the requested output directory. Do not inspect credentials, other sessions, source code, or observer logs.',
  'Finish by checking the saved scene and presentation. Give a concise truthful result with the actual paths and anything unfinished; no invented timings or call counts.',
].join('\n') : 'Make me a realistic banana in Blender using banana-reference.jpg. Work through the application interface, not scripts, code or modeling APIs. Save banana.blend and a polished banana.png in this workspace.');
await writeFile(join(output, 'request.txt'), prompt, {flag:'wx'});
await writeFile(join(output,'setup.json'),JSON.stringify({prompt,...auth,workspace,
  originalPrompt:recovery?.continuation?.originalPrompt||prompt,resumedFrom:prior||null,prepareOnly,
  mode:legacyGuided?'legacy-guided-ui':'plain-reference-native-ui',nativeGuiRequired:true,
  observerConstruction:false,observerCoaching:legacyGuided,referenceProvided:!legacyGuided,
  deadlineMinutes:30,model:'gpt-5.6-sol',reasoningEffort:'medium'},null,2),{flag:'wx'});

const loadScene=recovery?(recovery.continuation?recovery.source:join(workspace,'banana.blend')):null;
if([blender,workspace,loadScene||''].some(path=>/[\r\n"`$\\]/.test(path)))throw Error('Blender path contains unsupported Desktop Entry quoting characters');
const desktopEntry=[
  '[Desktop Entry]','Type=Application',`Name=${legacyGuided?'Phoenix Blender Acceptance':'Blender'}`,
  `Exec="${blender}" --disable-autoexec --window-geometry 0 0 1440 960 ${loadScene?`"${loadScene}"`:'--factory-startup'}`,
  'Terminal=false','',
].join('\n');
if(prepareOnly){
  await writeFile(join(output,'prepared-launch.desktop'),desktopEntry,{flag:'wx'});
  await writeFile(join(output,'preparation.json'),JSON.stringify({prepared:true,workspace,loadScene,
    preservedCheckpoint:recovery?.checkpoint||null,originalPrompt:recovery?.continuation?.originalPrompt||prompt,prompt,
    binarySha256:binaryHash,providerCalls:0,gatewayStarts:0,desktopStarts:0,
    scope:'Input/checkpoint/launcher preparation only. No model task was submitted and no authentication was attempted.'},null,2),{flag:'wx'});
  console.log('Native task prepared and checkpoint verified; no provider, gateway or desktop started.');
}else{

await withAcceptanceGateway({binary, output, workspace, ...auth, preserveState:true, setup:async ({home,env}) => {
  if(recovery){
    for(const directory of ['company','sessions','cas'])
      await cp(join(recovery.home,directory),join(home,directory),{recursive:true,errorOnExist:true,force:false});
  }
  const apps = join(env.XDG_DATA_HOME, 'applications'); await mkdir(apps, {recursive:true});
  await writeFile(join(apps, 'phoenix-blender-acceptance.desktop'),desktopEntry,{flag:'wx'});
}}, async ({socketPath, home}) => {
  await once(socketPath, {Onboarding:{action:'choose_company', choice:'founding_company'}});
  const view = await once(socketPath, {CompanyDirectory:{action:'status'}});
  const actor = view.CompanyDirectory?.directory.agents.find(a => a.internal_role === 'coder');
  if (!actor?.canonical_session_id) throw Error('The disposable coder has no canonical conversation; no task submitted');
  const sessionId = actor.canonical_session_id, agentId = actor.agent_id;
  const turnId = 'blender-ui-' + Date.now();
  await writeFile(join(output, 'actor.json'), JSON.stringify(actor, null, 2), {flag:'wx'});
  const log = await open(join(output, 'events.jsonl'), 'wx', 0o600);
  const started = Date.now(); let terminal, cancelSent = false;
  const stories = [], screenshots = new Set();
  const deadline = setTimeout(async () => {
    cancelSent = true;
    try {await once(socketPath, {Cancel:{session_id:sessionId, target_agent:agentId}});}
    catch (error) {console.error(JSON.stringify({phase:'cancel-observation-error', message:String(error)}));}
  }, 30 * 60 * 1000);
  try {
    await log.write(JSON.stringify({at:new Date().toISOString(), Submission:{sessionId, turnId}}) + '\n');
    console.log(JSON.stringify({phase:'submitted', sessionId, turnId, output}));
    for await (const value of messages(socketPath, {Turn:{session_id:sessionId, turn_id:turnId,
      user_request:prompt, workspace, interaction_mode:'execute', permission_mode:'full_access',
      target_agent:agentId, journal:true, delivery:'queue'}})) {
      await log.write(JSON.stringify({at:new Date().toISOString(), value}) + '\n');
      if (value.Story) {
        const story = value.Story; stories.push(story);
        if (['tool','status','answer','warning','compaction'].includes(story.kind))
          console.log(JSON.stringify({elapsed_ms:Date.now()-started, ...story}));
        for (const match of JSON.stringify(story).matchAll(/(?:\/[^\s"\\]+)+\.(?:png|jpg)/g))
          if (match[0].startsWith(home + '/')) screenshots.add(match[0]);
      }
      if (value.Done || value.Error) {terminal=value; break;}
    }
    if (!terminal) throw Error('Observation ended without terminal receipt; inspect this turn before submitting anything else');
    const calls = stories.filter(s => s.kind === 'tool');
    const savedSession = JSON.parse(await readFile(join(home, 'sessions', `${sessionId}.json`), 'utf8'));
    const {receipts, validations, audit} = auditBlenderTools(currentBlenderTurn(savedSession.messages, prompt), calls.length);
    await writeFile(join(output, 'tool-receipts.json'), JSON.stringify(receipts, null, 2), {flag:'wx', mode:0o600});
    await writeFile(join(output, 'validation-receipts.json'), JSON.stringify(validations, null, 2), {flag:'wx', mode:0o600});
    await writeFile(join(output, 'tool-audit.json'), JSON.stringify(audit, null, 2), {flag:'wx'});
    const fileSize = async path => {try {return (await stat(path)).size;} catch {return 0;}};
    const metrics = {elapsed_ms:Date.now()-started, tool_calls:calls.length,
      failed_tool_calls:receipts.filter(receipt => receipt.success === false).length,
      validation_events:audit.validation_events,
      failed_validation_events:audit.failed_validation_events,
      computer_tool_calls:calls.filter(c => c.tool?.startsWith('computer_')).length,
      all_tools_allowed:audit.all_tools_allowed,
      prohibited_console_shortcut:audit.prohibited_console_shortcut,
      tool_input_audit_complete:audit.complete, cancelSent,
      blend_bytes:await fileSize(join(workspace, 'banana.blend')),
      preview_bytes:await fileSize(join(workspace, 'banana.png')), terminal};
    if(recovery){
      const unchanged=await hashFile(recovery.checkpoint||recovery.source)===recovery.hash;
      metrics[recovery.checkpoint?'recovery_checkpoint_unchanged':'recovery_source_unchanged']=unchanged;
      if(!unchanged)throw Error('Protected recovery checkpoint changed');
    }
    metrics.checks=nativeCompletionChecks(metrics);
    await writeFile(join(output, 'metrics.json'), JSON.stringify(metrics, null, 2), {flag:'wx'});
    // Preserve original observed screens for independent visual review. This
    // copies evidence only; it never creates the agent's deliverable preview.
    let copied = 0; await mkdir(join(output, 'screens'), {recursive:true, mode:0o700});
    for (const path of screenshots) {
      try {await copyFile(path, join(output, 'screens', `${++copied}-${path.split('/').at(-1)}`));}
      catch (error) {if (error.code !== 'ENOENT') throw error;}
    }
    copied += (await collectDesktopEvidence(home, join(output, 'screens', 'observed'))).length;
    console.log(JSON.stringify({phase:'terminal', ...metrics, screensCopied:copied}));
    if (!Object.values(metrics.checks).every(Boolean))
      process.exitCode = 1;
    // File presence is deliberately not the visual/geometry acceptance gate.
  } finally {clearTimeout(deadline); await log.close();}
});
}
