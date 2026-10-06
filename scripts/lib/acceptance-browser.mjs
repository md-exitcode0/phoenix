// Real packaged Electron browser in a disposable X display and Phoenix home.
// Shared by visual acceptance runs; no host browser, cookies or window is used.
import {spawn} from 'node:child_process';
import {open,writeFile,readFile,readdir} from 'node:fs/promises';
import {randomBytes} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {join,dirname} from 'node:path';
import net from 'node:net';

export async function connectCdp(url) {
  const socket=new WebSocket(url),pending=new Map();let sequence=0;
  await new Promise((resolve,reject)=>{socket.addEventListener('open',resolve,{once:true});socket.addEventListener('error',()=>reject(Error('CDP connection failed')),{once:true});});
  socket.addEventListener('message',event=>{
    const value=JSON.parse(String(event.data)),call=pending.get(value.id);if(!call)return;
    pending.delete(value.id);clearTimeout(call.timer);
    value.error?call.reject(Error(value.error.message)):call.resolve(value.result);
  });
  socket.addEventListener('close',()=>{for(const call of pending.values()){clearTimeout(call.timer);call.reject(Error('CDP connection closed'));}pending.clear();});
  return {
    send(method,params={}){const id=++sequence;return new Promise((resolve,reject)=>{
      const timer=setTimeout(()=>{pending.delete(id);reject(Error(`${method} timed out`));},20000);
      pending.set(id,{resolve,reject,timer});socket.send(JSON.stringify({id,method,params}));
    });},
    async evaluate(expression){const value=await this.send('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true});if(value.exceptionDetails)throw Error(value.exceptionDetails.exception?.description||value.exceptionDetails.text);return value.result?.value;},
    close(){socket.close();}
  };
}

async function freePort() {
  const server=net.createServer();
  await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(0,'127.0.0.1',resolve);});
  const port=server.address().port;
  await new Promise((resolve,reject)=>server.close(error=>error?reject(error):resolve()));
  return port;
}

// xvfb-run can exit before Electron's children finish writing their profile.
// Keep teardown bounded, but do not use the wrapper's exit as proof that the
// detached process group is gone. This helper is only for groups we spawned.
export async function stopOwnedProcessGroup(child,exited,{graceMs=5000}={}) {
  const group=child?.pid;
  if(!group)return;
  if(!Number.isSafeInteger(group)||group<=1||group===process.pid)throw Error('Invalid owned process group');
  const signal=value=>{try{process.kill(-group,value);}catch(error){if(error.code!=='ESRCH')throw error;}};
  const liveMembers=async()=>{
    try{process.kill(-group,0);}catch(error){if(error.code==='ESRCH')return [];throw error;}
    const rows=await Promise.all((await readdir('/proc')).filter(name=>/^\d+$/.test(name)).map(async name=>{
      try{
        const stat=await readFile(`/proc/${name}/stat`,'utf8'),fields=stat.slice(stat.lastIndexOf(')')+2).trim().split(/\s+/);
        return Number(fields[2])===group&&!['Z','X'].includes(fields[0])?Number(name):null;
      }catch(error){if(['ENOENT','ESRCH'].includes(error.code))return null;throw error;}
    }));
    return rows.filter(pid=>pid!==null);
  };
  const wait=async ms=>{
    const deadline=Date.now()+ms;
    do{
      if(!(await liveMembers()).length){
        // Recheck a quiescent group so a child forked during /proc enumeration
        // cannot make a disappearing wrapper look like completed cleanup.
        await new Promise(resolve=>setTimeout(resolve,30));
        if(!(await liveMembers()).length)return true;
      }
      await new Promise(resolve=>setTimeout(resolve,30));
    }while(Date.now()<deadline);
    return false;
  };
  signal('SIGTERM');
  if(!await wait(graceMs)){
    signal('SIGKILL');
    if(!await wait(2000))throw Error(`Owned process group ${group} did not stop; its profile must be retained`);
  }
  let timer;
  try{
    await Promise.race([exited,new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error('Owned wrapper did not reap')),2000);})]);
  }finally{clearTimeout(timer);}
}

export async function acceptanceBrowser({home,env,output}) {
  const shell=fileURLToPath(new URL('../../canvas-app/chromium-shell/',import.meta.url));
  const electron=join(shell,'node_modules/electron/dist/electron');
  const bridgePort=await freePort(),debugPort=await freePort();
  const token=randomBytes(32).toString('hex');
  env.PHOENIX_CHROMIUM_BRIDGE_URL=`http://127.0.0.1:${bridgePort}/`;
  env.PHOENIX_CHROMIUM_BRIDGE_TOKEN=token;
  env.PATH=dirname(process.execPath)+':'+env.PATH;
  const log=await open(join(output,'browser.log'),'wx',0o600);
  let child,exit,exitReceipt,stopRequested=false;
  const launchedAt=Date.now();
  const stop=async()=>{
    stopRequested=true;
    try{
      if(child)await stopOwnedProcessGroup(child,exit);
      if(exitReceipt)await exitReceipt;
    }finally{
      await log.close();
    }
  };
  try {
    child=spawn('xvfb-run',['--auto-servernum','--server-args=-screen 0 1440x960x24 -nolisten tcp',electron,shell],{
      cwd:shell,detached:true,stdio:['ignore',log.fd,log.fd],
      env:{...env,PHOENIX_HOME:home,PHOENIX_CHROMIUM_BRIDGE_PORT:String(bridgePort),
        PHOENIX_CHROMIUM_DEBUG_PORT:String(debugPort),PHOENIX_CHROMIUM_OZONE_PLATFORM:'x11',
        PHOENIX_RUST_PARENT_PID:String(process.pid)}});
    exit=new Promise((resolve,reject)=>{child.once('error',reject);child.once('exit',(code,signal)=>resolve({code,signal,
      observedAt:new Date().toISOString(),elapsedMs:Date.now()-launchedAt,stopRequested}));});
    exit.catch(()=>{});
    // Record unexpected child termination when it happens, not only when the
    // task eventually shuts down. Startup readiness is not lifetime evidence.
    exitReceipt=exit.then(value=>writeFile(join(output,'browser-exit.json'),JSON.stringify({pid:child.pid,...value},null,2),{flag:'wx',mode:0o600}));
    exitReceipt.catch(()=>{});
    const endpoint=new URL('health',env.PHOENIX_CHROMIUM_BRIDGE_URL);endpoint.searchParams.set('token',token);
    let health;
    for(let n=0;n<100;n++){
      if(child.exitCode!==null||child.signalCode!==null)throw Error('Owned Electron browser exited before readiness; inspect browser.log');
      try { const response=await fetch(endpoint,{signal:AbortSignal.timeout(1000)});if(response.ok){health=await response.json();if(health.ok)break;} }catch{}
      await new Promise(resolve=>setTimeout(resolve,100));
    }
    if(!health?.ok)throw Error('Owned Electron browser did not become ready');
    await writeFile(join(output,'browser.json'),JSON.stringify({home,pid:child.pid,debugPort,bridgePort,engine:health.engine,isolatedDisplay:true},null,2),{flag:'wx'});
    return stop;
  } catch(error) {await stop();throw error;}
}
