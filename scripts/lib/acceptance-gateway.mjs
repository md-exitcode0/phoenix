import {acceptanceAccounts,codexLoginAccount,acceptanceAccountProvenance} from './acceptance-account.mjs';
// Real-provider acceptance support. All writes, sessions, and gateway lifetime
// belong to one disposable Phoenix home; the user's daemon is never contacted.
import net from 'node:net';
import {spawn, execFileSync} from 'node:child_process';
import {mkdtemp, mkdir, readFile, writeFile, unlink, open, cp, lstat} from 'node:fs/promises';
import {join} from 'node:path';
import {homedir, tmpdir} from 'node:os';

export const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

export async function* messages(socketPath, body, {signal} = {}) {
  const socket = net.createConnection(socketPath);
  socket.setEncoding('utf8');
  const abort = () => socket.destroy(signal.reason || Error('Acceptance observation aborted'));
  if (signal?.aborted) abort();
  else signal?.addEventListener('abort', abort, {once:true});
  socket.once('connect', () => socket.write(JSON.stringify(body) + '\n'));
  let pending = '';
  try {
    for await (const chunk of socket) {
      pending += chunk;
      if (Buffer.byteLength(pending) > 16 * 1024 * 1024) throw Error('Gateway event exceeds acceptance capture bound');
      let index;
      while ((index = pending.indexOf('\n')) >= 0) {
        const line = pending.slice(0, index); pending = pending.slice(index + 1);
        if (line.trim()) yield JSON.parse(line);
      }
    }
    if (pending.trim()) throw Error('Gateway closed with an incomplete event; inspect the existing turn before taking action');
  } finally {
    signal?.removeEventListener('abort', abort);
    socket.destroy();
  }
}

export async function once(socketPath, body, timeoutMs = 10000) {
  for await (const value of messages(socketPath, body, {signal:AbortSignal.timeout(timeoutMs)})) {
    if (value.Error) throw Error(JSON.stringify(value.Error));
    return value;
  }
  throw Error('Gateway closed without a response; do not automatically resubmit');
}

export async function withAcceptanceGateway({binary, output, workspace, setup, memory = false, preserveState = false, accountPool = false,
  visionModel = null, nativeVision = true, authSource = 'phoenix', authProfile = null}, run) {
  if (!['phoenix','codex'].includes(authSource)) throw Error('Unknown acceptance authentication source');
  if (Number(accountPool) + Number(Boolean(authProfile)) + Number(authSource === 'codex') > 1)
    throw Error('Choose one acceptance authentication source');
  if (authProfile !== null && (typeof authProfile !== 'string' || !/^[a-zA-Z0-9][a-zA-Z0-9:._-]{0,159}$/.test(authProfile)))
    throw Error('Acceptance profile must be an exact saved profile ID');
  const sourceHome = process.env.PHOENIX_HOME || join(homedir(), '.phoenix');
  const model = process.env.PHOENIX_ACCEPTANCE_MODEL || 'gpt-6-sol';
  if (!/^[a-z0-9][a-z0-9.:-]{0,79}$/.test(model)) throw Error('Invalid PHOENIX_ACCEPTANCE_MODEL');
  const home = await mkdtemp(join(tmpdir(), 'phoenix-ui-acceptance-'));
  const authPath = join(home, 'auth-profiles.json');
  const socketPath = join(home, 'gateway.sock');
  const started = Date.now();
  let gateway, gatewayDone, streams = [], error, setupCleanup, stopping=false, selectedAccounts=[];
  const processLog = await open(join(output, 'gateway.log'), 'wx', 0o600);
  const env = {...process.env, PHOENIX_HOME:home, PHOENIX_WS_PORT:'0',
    XDG_DATA_HOME:join(home, 'data'), XDG_CONFIG_HOME:join(home, 'config'),
    XDG_CACHE_HOME:join(home, 'cache'), BLENDER_USER_RESOURCES:join(home, 'blender'),
    TMPDIR:join(home, 'tmp'), PHOENIX_BROWSER_LOGIN_SOURCE:'none'};
  if (!memory) env.PHOENIX_NO_LIBRARIAN = '1';
  // No test may attach to a desktop inherited from the user's session.
  for (const key of ['DISPLAY','WAYLAND_DISPLAY','DBUS_SESSION_BUS_ADDRESS',
    'DBUS_STARTER_ADDRESS','DBUS_STARTER_BUS_TYPE','XDG_ACTIVATION_TOKEN',
    'PHOENIX_CHROMIUM_BRIDGE_URL','PHOENIX_CHROMIUM_BRIDGE_TOKEN','PHOENIX_BROWSER_ATTACH']) delete env[key];
  const terminate = () => {
    stopping=true;
    if (gateway && gateway.exitCode === null && gateway.signalCode === null) gateway.kill('SIGTERM');
  };
  process.once('SIGINT', terminate); process.once('SIGTERM', terminate);
  try {
    let accounts, sourceAuth;
    if (authSource === 'codex') {
      accounts = [codexLoginAccount(JSON.parse(await readFile(join(homedir(), '.codex', 'auth.json'), 'utf8')))];
    } else {
    const auth = sourceAuth = JSON.parse(await readFile(join(sourceHome, 'auth-profiles.json'), 'utf8'));
    // Follow the configured connection by default. An explicitly requested
    // isolated pool test uses the saved provider priority without changing
    // the user config or copying refresh tokens.
    const selected = JSON.parse(execFileSync('python3', ['-c',
      'import json,tomllib,sys; c=tomllib.load(open(sys.argv[1],"rb")); l=c.get("profile",{}).get("llm",{}); print(json.dumps({"provider":l.get("provider"),"profile":l.get("auth",{}).get("profile")}))',
      join(sourceHome, 'config.toml')], {encoding:'utf8'}));
    accounts = acceptanceAccounts(auth, accountPool ? undefined : authProfile || selected.profile, Date.now(), selected.provider);
    }
    selectedAccounts = acceptanceAccountProvenance(sourceAuth, accounts, authSource);
    await writeFile(join(output, 'account-selection.json'), JSON.stringify({authSource, accountPool, authProfile,
      sourceHome:authSource === 'phoenix' ? sourceHome : null, selectedAccounts,
      meaning:'Selected access credentials only; availability and serving-account outcomes require actual provider receipts.'}, null, 2), {mode:0o600, flag:'wx'});
    // Never duplicate refresh tokens, other accounts, or user conversation data.
    const profiles = Object.fromEntries(accounts.map((saved,index) => [index ? `probe-${index+1}` : 'probe',
      {type:'token',provider:'openai-codex',token:saved.access || saved.token,expires:saved.expires}]));
    await writeFile(authPath, JSON.stringify({version:1, profiles,
      state:{order:{'provider:openai-codex':Object.keys(profiles)}}}), {mode:0o600, flag:'wx'});
    await writeFile(join(home, 'config.toml'), [
      '[profile]', 'name="computer-team-acceptance"', '[profile.llm]',
        'provider="openai-codex"', `model="${model}"`, `specialist_model="${model}"`,
      `librarian_model="${model}"`, `native_vision=${nativeVision}`,
      ...(visionModel ? [`vision_model="${visionModel}"`, 'vision_provider="openai-codex"'] : []),
      'reasoning_effort="medium"',
      ...(visionModel ? ['[profile.llm.efforts]', 'vision="low"'] : []),
      '[profile.llm.auth]', 'source="profile"', 'profile="probe"', '',
    ].join('\n'), {mode:0o600, flag:'wx'});
    await writeFile(join(home, 'settings.json'), JSON.stringify({version:1, revision:1,
      updated_at:new Date().toISOString(), migrated_canvas_preferences:true,
      global:{'memory.enabled':memory}, agents:{}, groups:{}}), {mode:0o600, flag:'wx'});
    for (const key of ['XDG_DATA_HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','BLENDER_USER_RESOURCES','TMPDIR'])
      await mkdir(env[key], {recursive:true, mode:0o700});
    setupCleanup = await setup?.({home, env});
    if(stopping)throw Error('Acceptance interrupted during setup; no task submitted');
    gateway = spawn(binary, [], {cwd:workspace, env, stdio:['ignore','pipe','pipe']});
    gatewayDone = new Promise((resolve, reject) => {
      gateway.once('error', reject);
      gateway.once('exit', (code, signal) => resolve({code, signal}));
    });
    // Register rejection immediately even if startup fails before the final await.
    gatewayDone.catch(() => {});
    const capture = async stream => {for await (const bytes of stream) await processLog.write(bytes);};
    streams = [capture(gateway.stdout), capture(gateway.stderr)];
    let ready = false;
    for (let attempt = 0; attempt < 100; attempt++) {
      if (gateway.exitCode !== null || gateway.signalCode !== null) throw Error('Owned gateway exited before readiness');
      try {
        const response = await once(socketPath, 'Ping', 1000);
        if (response === 'Pong' || response?.Pong !== undefined) {ready = true; break;}
      } catch {}
      await sleep(100);
    }
    if (!ready) throw Error('Owned gateway did not become ready; no task submitted');
    if(stopping)throw Error('Acceptance interrupted before submission');
    console.log(JSON.stringify({phase:'gateway-ready', home, pid:gateway.pid, model, reasoning:'medium', visionModel, nativeVision,
      authSource, accountPool, selectedAccounts}));
    return await run({home, socketPath, env, gateway});
  } catch (caught) {
    error = String(caught?.stack || caught); throw caught;
  } finally {
    process.removeListener('SIGINT', terminate); process.removeListener('SIGTERM', terminate);
    const cleanupFailures=[];
    const cleanup=async(label,operation)=>{try{await operation();}catch(caught){cleanupFailures.push({label,error:String(caught?.message||caught)});}};
    await cleanup('gateway signal',async()=>terminate());
    if (gatewayDone) {
      const killTimer = setTimeout(() => {
        if (gateway.exitCode === null && gateway.signalCode === null) gateway.kill('SIGKILL');
      }, 10000);
      try {await cleanup('gateway exit',()=>gatewayDone);} finally {clearTimeout(killTimer);}
    }
    const captured=await Promise.allSettled(streams);
    for(const result of captured)if(result.status==='rejected')cleanupFailures.push({label:'gateway log capture',error:String(result.reason)});
    await cleanup('gateway log close',()=>processLog.close());
    let credentialRemoved=false;
    await cleanup('access credential removal',async()=>{
      await unlink(authPath).catch(e=>{if(e.code!=='ENOENT')throw e;});credentialRemoved=true;
    });
    await cleanup('test resource shutdown',async()=>{await setupCleanup?.();});
    let durableState=null,snapshotError=null;
    if(preserveState&&gateway?.exitCode===0){
      const destination=join(output,'durable-state');
      try{
        await mkdir(destination,{mode:0o700});
        // Only stopped test-company records. Never copy the home wholesale:
        // credentials, configuration, sockets and desktop profiles stay out.
        for(const directory of ['company','sessions','cas','runs']) {
          // A pre-submission failure need not have initialized every store.
          // Ignore only an absent top-level store, not errors during its copy.
          const source=join(home,directory);
          const info=await lstat(source).catch(error=>{if(error.code==='ENOENT')return null;throw error;});
          if(info)await cp(source,join(destination,directory),{recursive:true,errorOnExist:true,force:false});
        }
        durableState=destination;
      }catch(caught){snapshotError=caught;}
    }
    await cleanup('execution receipt',()=>writeFile(join(output, 'execution.json'), JSON.stringify({home, elapsed_ms:Date.now()-started,
      model, reasoning_effort:'medium', visionModel, vision_reasoning_effort:visionModel?'low':null,
      nativeVision, memory, accountPool, authSource, authProfile, selectedAccounts,
      gateway:gateway ? {pid:gateway.pid, code:gateway.exitCode, signal:gateway.signalCode} : null,
      credentialRemoved, durableState, cleanupFailures, snapshotError:snapshotError?String(snapshotError):null, error:error || null}, null, 2), {mode:0o600, flag:'wx'}));
    if(snapshotError&&!error)throw snapshotError;
    if(cleanupFailures.length&&!error)throw Error('Acceptance cleanup failed: '+JSON.stringify(cleanupFailures));
  }
}
