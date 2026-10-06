// Run: dbus-run-session -- gjs -m scripts/check-cursor-dbus.mjs
// Real GJS/Gio/GLib dispatch and timers; pointer/compositor boundaries stubbed.
// The private bus cannot address the user's installed extension.
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import {parseKeyCombo, KEYSYMS, keypadKeycode, validateWindowActions} from '../desktop/gnome-extension/phoenix-cursor@phoenix.dev/cursor-input.mjs';

const [, bytes] = GLib.file_get_contents('desktop/gnome-extension/phoenix-cursor@phoenix.dev/extension.js');
const source = new TextDecoder().decode(bytes);
const xml = source.match(/const IFACE_XML = `([\s\S]*?)`;/)[1];
let point = [55,66];
const desktop={get_pointer:()=>[...point],display:{focus_window:null}};
let nativeCallback=null, nativeStream=null;
let screenshotFailure=false;
const outputDir=GLib.dir_make_tmp('phoenix-cursor-publication-XXXXXX');
const outputRoot=Gio.File.new_for_path(outputDir);
const files=()=>{
    const reader=outputRoot.enumerate_children('standard::name',Gio.FileQueryInfoFlags.NONE,null);
    const names=[];let entry;
    while((entry=reader.next_file(null)))names.push(entry.get_name());
    reader.close(null);return names;
};
const captureShell={Screenshot:class {
    screenshot_window(_a,_b,stream,callback){
        nativeStream=stream;
        stream.write_all(new TextEncoder().encode('partial-'),null);
        nativeCallback=()=>{
            stream.write_all(new TextEncoder().encode('complete'),null);
            callback(null,{});
        };
    }
    screenshot_window_finish(){return [true,null];}
    screenshot(_cursor,stream,callback){this.screenshot_window(false,false,stream,callback);}
    screenshot_finish(){if(screenshotFailure)throw new Error('fixture native failure');return [true];}
}};
const Cursor = new Function('Extension','Clutter','GLib','global','Gio','Shell','Main','parseKeyCombo','KEYSYMS','keypadKeycode','validateWindowActions',
    source.replace(/^import .*;\n/gm,'').replace('export default class','return class'))(
    class {}, {BUTTON_PRIMARY:1,ButtonState:{PRESSED:1,RELEASED:0},KeyState:{PRESSED:1,RELEASED:0}}, GLib,
    desktop,Gio,captureShell,{activateWindow:win=>{desktop.display.focus_window=win;}},parseKeyCombo,KEYSYMS,keypadKeycode,validateWindowActions);
const cursor = new Cursor();
cursor._timeouts = new Set();
cursor._restorePointer = true;
cursor._cursorCenter = () => [7,8];
cursor._pointerTo = (x,y) => {point=[x,y];};
cursor._spawnRipple = () => {};
cursor._now = () => 0;
cursor._vpointer = {notify_button:()=>{}};
const connection = Gio.bus_get_sync(Gio.BusType.SESSION,null);
const exported = Gio.DBusExportedObject.wrapJSObject(xml,cursor);
exported.export(connection,'/dev/phoenix/Cursor');
const loop = new GLib.MainLoop(null,false);
const ensure = (condition,message) => {if(!condition)throw new Error(message);};
function call(method, signature, args, timeout=2000) {
    return new Promise((resolve,reject)=>connection.call(connection.get_unique_name(),
        '/dev/phoenix/Cursor','dev.phoenix.Cursor',method,new GLib.Variant(signature,args),
        null,Gio.DBusCallFlags.NONE,timeout,null,(conn,result)=>{
            try {resolve(conn.call_finish(result));} catch(error){reject(error);}
        }));
}
function delay(ms) {return new Promise(resolve=>GLib.timeout_add(GLib.PRIORITY_DEFAULT,ms,()=>{
    resolve();return GLib.SOURCE_REMOVE;
}));}
let failed = false;
async function verify() {
    cursor._showCursor=()=>{};
    let invalidInput=0;
    cursor._vkeyboard={notify_keyval:()=>invalidInput++};
    cursor._windowById=()=>({win:{}});
    for(const combo of ['ctrl+typo','ctrl++a','a+b','ctrl+control+a','constructor','ctrl+toString','a+']) {
        let rejected=false;
        try {await call('Key','(s)',[combo]);} catch(_){rejected=true;}
        ensure(rejected && invalidInput===0,'malformed shortcut emitted input or was accepted');
        const batch=JSON.parse((await call('WindowBatch','(us)',[42,JSON.stringify([
            {type:'type',text:'must not run'}, {type:'key',combo},
        ])])).deepUnpack()[0]);
        ensure(batch.ok===false && batch.executed===0 && batch.error.includes('action #2'),'invalid batch not rejected before execution');
        ensure(invalidInput===0 && !cursor._batch,'invalid batch emitted input or acquired ownership');
    }
    print('Keyboard preflight: seven malformed shortcuts and batches rejected through real D-Bus without input');
    for(const failAt of [-1,0,1,2,3,4,5]) {
        const events=[],held=new Set();
        cursor._vkeyboard={notify_keyval:(_,key,state)=>{
            events.push([key,state]);
            if(state)held.add(key);else held.delete(key);
            if(events.length-1===failAt)throw new Error('injected keyboard failure');
        }};
        let failed=false,wireError='';
        try {await call('Key','(s)',['ctrl+shift+a']);}
        catch(error){wireError=String(error);failed=wireError.includes('injected keyboard failure');}
        ensure(failed===(failAt!==-1),`wrong keyboard wire result at ${failAt}: ${wireError}`);
        ensure(held.size===0,'keyboard failure left modeled keys held');
        const presses=events.filter(([,state])=>state===1).map(([key])=>key).reverse();
        const releases=events.filter(([,state])=>state===0).map(([key])=>key);
        ensure(JSON.stringify(presses)===JSON.stringify(releases),'incomplete keyboard release attempts');
    }
    print('Keyboard: real D-Bus success and six injected press/release failures all unwind attempted keys');
    // Before-control: the original direct replace API can overwrite an
    // existing fixture when native output closes after cancellation.
    const oldPath=`${outputDir}/before-direct-replace.fixture`;
    GLib.file_set_contents(oldPath,'original-output');
    const oldStream=Gio.File.new_for_path(oldPath).replace(null,false,Gio.FileCreateFlags.NONE,null);
    oldStream.write_all(new TextEncoder().encode('late-canceled-output'),null);
    oldStream.close(null);
    const [,oldBytes]=GLib.file_get_contents(oldPath);
    ensure(new TextDecoder().decode(oldBytes)==='late-canceled-output','before-control did not reproduce overwrite');
    print('Before-control: original direct replacement overwrote existing output on late close');
    for (const method of ['Click','DoubleClick','Scroll']) {
        const start = GLib.get_monotonic_time();
        let settled=false;
        const completion=call(method,method==='Scroll'?'(ii)':'(s)',method==='Scroll'?[0,0]:['left'])
            .then(result=>{settled=true;return result;});
        await delay(25);
        ensure(!settled,`${method}: premature completion`);
        ensure(point[0]===7 && point[1]===8,`${method}: action position missing`);
        let busy=false;
        try { await call('MoveTo','(iiu)',[10,20,100]); }
        catch(error) {busy=String(error).includes('dev.phoenix.Cursor.Busy');}
        ensure(busy,`${method}: competing operation not rejected as Busy`);
        await completion;
        const elapsed=(GLib.get_monotonic_time()-start)/1000;
        ensure(elapsed>=100,`${method}: restoration interval skipped`);
        ensure(point[0]===55 && point[1]===66,`${method}: reply preceded restore`);
        ensure(!cursor._pointerAction && cursor._timeouts.size===0,`${method}: leaked lease/timer`);
        print(`${method}: real D-Bus completion after restore (${elapsed.toFixed(1)} ms), concurrent MoveTo rejected`);
    }
    for (const cancel of [false,true]) {
        const buttons=[];
        cursor._vpointer.notify_button=(_,button,state)=>{
            buttons.push(state);
            if(!cancel && state===1) throw new Error('uncertain press');
        };
        const completion=call('Click','(s)',['left']).then(()=>null,error=>error);
        await delay(25);
        if(cancel) await call('Hide','()',[]);
        const error=await completion;
        ensure(error && String(error).includes('dev.phoenix.Cursor.Canceled'),'missing terminal error');
        ensure(String(error).includes(cancel?'Cursor hidden':'uncertain press'),'wrong failure cause');
        ensure(buttons.join(',')==='1,0','button release missing');
        ensure(point[0]===55 && point[1]===66,'failure reply preceded restore');
        ensure(!cursor._pointerAction && cursor._timeouts.size===0,'failure leaked lease/timer');
        print(`${cancel?'Hide cancellation':'Uncertain press'}: real wire error after restore, button released`);
    }
    cursor._userIdleMs=()=>1000;
    const win={get_buffer_rect:()=>({x:0,y:0,width:100,height:80}),
        get_frame_rect:()=>({x:0,y:0,width:100,height:80}),get_title:()=> 'fixture',get_wm_class:()=> 'fixture'};
    cursor._windowById=()=>({win});
    for(const mode of ['before-paint','in-flight','success','collision']) {
        nativeCallback=null;
        const path=`${outputDir}/${mode}.fixture`;
        if(mode==='collision')GLib.file_set_contents(path,'original-output');
        const completion=call('CaptureWindow','(us)',[42,path]);
        await delay(mode==='before-paint'?25:300);
        if(mode==='before-paint'||mode==='in-flight') await call('Hide','()',[]);
        if(mode==='before-paint') {
            await delay(300);
            ensure(!nativeCallback && !files().some(name=>name.startsWith(mode)),'canceled paint still started capture');
        } else {
            ensure(nativeCallback,'native capture not reached');
            const staged=files().find(name=>name.startsWith(`${mode}.fixture.phoenix-`));
            ensure(staged,'private staging file missing');
            const permissions=outputRoot.get_child(staged).query_info('unix::mode',Gio.FileQueryInfoFlags.NONE,null).get_attribute_uint32('unix::mode');
            ensure((permissions & 0o777)===0o600,'staging not private');
            if(mode!=='collision')ensure(!Gio.File.new_for_path(path).query_exists(null),'output published before callback');
            if(mode==='in-flight') {
                ensure(cursor._batch,'native capture released ownership too early');
                let busy=false;
                try {await call('MoveTo','(iiu)',[1,2,100]);}
                catch(error){busy=String(error).includes('dev.phoenix.Cursor.Busy');}
                ensure(busy,'native drain admitted competing move');
            }
            nativeCallback();
            ensure(nativeStream.is_closed(),'capture stream leaked');
        }
        const result=JSON.parse((await completion).deepUnpack()[0]);
        ensure(result.ok===(mode==='success'),'wrong capture result');
        ensure(!cursor._batch && cursor._timeouts.size===0,'capture retained lease/timer after drain');
        ensure(!files().some(name=>name.endsWith('.partial')),'staging file leaked');
        if(mode==='success'||mode==='collision') {
            const [,data]=GLib.file_get_contents(path);
            ensure(new TextDecoder().decode(data)===(mode==='success'?'partial-complete':'original-output'),'wrong final bytes or overwritten destination');
        } else ensure(!Gio.File.new_for_path(path).query_exists(null),'canceled capture published late output');
        print(`Capture ${mode}: real private-file staging/publication, wire result and timer cleanup passed`);
    }
    for(const mode of ['success','collision','cancel','failure','timeout','disable']) {
        const path=`${outputDir}/fullscreen-${mode}.fixture`;
        screenshotFailure=mode==='failure';
        if(mode==='collision')GLib.file_set_contents(path,'original-output');
        nativeCallback=null;
        const completion=call('Screenshot','(s)',[path],15000);
        await delay(25);
        ensure(nativeCallback,'full-screen capture not started');
        ensure(cursor._screenshots.size===1,'full-screen capture not tracked');
        const staged=files().find(name=>name.startsWith(`fullscreen-${mode}.fixture.phoenix-`));
        ensure(staged,'full-screen staging missing');
        const permissions=outputRoot.get_child(staged).query_info('unix::mode',Gio.FileQueryInfoFlags.NONE,null).get_attribute_uint32('unix::mode');
        ensure((permissions&0o777)===0o600,'full-screen staging not private');
        if(mode!=='collision')ensure(!Gio.File.new_for_path(path).query_exists(null),'full-screen output exposed before completion');
        if(mode==='cancel')await call('Hide','()',[]);
        if(mode==='disable')cursor.disable();
        if(mode==='timeout') {
            const timed=(await completion).deepUnpack()[0];
            ensure(timed.includes('timed out'),'native deadline not reported');
            ensure(cursor._screenshots.size===1,'pending native capture lost tracking');
            ensure(!Gio.File.new_for_path(path).query_exists(null),'timeout published output');
        }
        nativeCallback();
        const result=(await completion).deepUnpack()[0];
        ensure(mode==='success'?result==='ok':result.startsWith('error:'),'wrong full-screen result');
        ensure(nativeStream.is_closed(),'full-screen stream leaked');
        ensure(cursor._screenshots.size===0 && cursor._timeouts.size===0,'full-screen ownership/timer leaked');
        ensure(!files().some(name=>name.endsWith('.partial')),'full-screen staging leaked');
        if(mode==='success'||mode==='collision') {
            const [,data]=GLib.file_get_contents(path);
            ensure(new TextDecoder().decode(data)===(mode==='success'?'partial-complete':'original-output'),'full-screen bytes wrong');
        } else ensure(!Gio.File.new_for_path(path).query_exists(null),'failed full-screen capture published');
        print(`Full-screen ${mode}: real filesystem publication and wire result passed`);
    }
    print(`Synthetic filesystem evidence retained: ${outputDir}`);
}
verify().catch(error=>{failed=true;printerr(`${error}\n${error.stack || ''}`);}).finally(()=>{
    exported.unexport();loop.quit();
});
loop.run();
if(failed) throw new Error('Cursor D-Bus verification failed');
print('CURSOR_DBUS_OK: real GJS async export, wire replies and GLib timer ownership; no live compositor claim');
