'use strict';
// Actual UI initializer; fake native calls never connect channels.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const source=fs.readFileSync(path.join(__dirname,'../review-shell/ui/channels.js'),'utf8');
(async()=>{
 let passed=0;
 for(const paused of [false,true]){
  const calls=[],timers=[];
  const window={PhoenixIsolatedBackend:{backgroundPaused:paused},PhoenixUI:{TAURI:true,SIDEBAR_PREVIEW:false,escapeHtml:String,invoke:async(command,args)=>{calls.push(args.action);return {connections:[{config:{id:'fixture',enabled:true},status:{saved_login:true,running:false}}]};}}};
  vm.runInNewContext(source,{window,setTimeout(fn){timers.push(fn);},localStorage:{getItem(){return null;}}});
  assert.equal(timers.length,1);passed++;
  await timers[0]();
  assert.deepEqual(calls,paused?[]:['list','start']);passed++;
  assert.equal(typeof window.PhoenixChannels.render,'function');passed++;
 }
 console.log(JSON.stringify({passed,scope:'Actual channels initializer: ordinary startup still reconnects; inspection pause issues no channel/service calls.'}));
})().catch(e=>{console.error(e);process.exitCode=1});
