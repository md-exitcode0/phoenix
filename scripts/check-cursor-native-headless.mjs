// Real GNOME compositor smoke test on a private bus, runtime and settings root.
// Never issue input on the user's display or restart their Shell.
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,cp,writeFile,readFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
const script=fileURLToPath(import.meta.url);
if(process.env.PHOENIX_NATIVE_BLENDER_FILE_DIALOG==='1'&&process.env.PHOENIX_TEST_BLENDER_INPUT!=='1')
  throw Error('File-dialog acceptance requires PHOENIX_TEST_BLENDER_INPUT=1');
const run=(command,args,env=process.env)=>new Promise((resolve,reject)=>{
  const child=spawn(command,args,{env,stdio:['ignore','pipe','pipe']});let stdout='',stderr='';
  child.stdout.on('data',data=>stdout+=data);child.stderr.on('data',data=>stderr+=data);
  child.on('error',reject);child.on('exit',(code,signal)=>resolve({code,signal,stdout,stderr}));
});
if(process.argv[2]!=='--private') {
  const root=await mkdtemp(join(tmpdir(),'phoenix-native-cursor-'));
  for(const name of ['data','config','cache','runtime'])await mkdir(join(root,name),{mode:0o700});
  await cp(resolve('desktop/gnome-extension/phoenix-cursor@phoenix.dev'),join(root,'data/gnome-shell/extensions/phoenix-cursor@phoenix.dev'),{recursive:true});
  const env={...process.env,XDG_DATA_HOME:join(root,'data'),XDG_CONFIG_HOME:join(root,'config'),XDG_CACHE_HOME:join(root,'cache'),XDG_RUNTIME_DIR:join(root,'runtime'),GSETTINGS_BACKEND:'keyfile',LIBGL_ALWAYS_SOFTWARE:'1',PHOENIX_DESKTOP_SCOPE:'desktop-'+createHash('sha256').update(root).digest('hex').slice(0,24)};
  for(const name of ['DISPLAY','WAYLAND_DISPLAY','DBUS_SESSION_BUS_ADDRESS','DBUS_STARTER_ADDRESS','DBUS_STARTER_BUS_TYPE'])delete env[name];
  const result=await run('dbus-run-session',['--',process.execPath,script,'--private',root],env);
  await writeFile(join(root,'probe.log'),result.stdout+result.stderr,{mode:0o600});
  console.log(JSON.stringify({root,code:result.code,signal:result.signal,observations:result.stdout,log:join(root,'probe.log')}));process.exitCode=result.code===0?0:1;
} else {
  const root=process.argv[3];
  if(!root?.startsWith('/tmp/phoenix-native-cursor-')||process.env.XDG_RUNTIME_DIR!==join(root,'runtime')||!process.env.DBUS_SESSION_BUS_ADDRESS)throw Error('Private compositor scope missing');
  for(const [key,value] of [['enabled-extensions',"['phoenix-cursor@phoenix.dev']"],['disable-user-extensions','false']]) {
    const result=await run('gsettings',['set','org.gnome.shell',key,value]);
    if(result.code!==0)throw Error(result.stderr);
  }
  const mouseKeys=await run('gsettings',['set','org.gnome.desktop.a11y.keyboard','mousekeys-enable','false']);
  if(mouseKeys.code!==0)throw Error('Could not configure private keypad input');
  const shell=spawn('gnome-shell',['--headless','--wayland',...(process.env.PHOENIX_NATIVE_XWAYLAND==='1'?[]:['--no-x11']),'--virtual-monitor','1280x720','--wayland-display','phoenix-test'],{stdio:['ignore','pipe','pipe']});
  let shellLog='';shell.stdout.on('data',data=>shellLog+=data);shell.stderr.on('data',data=>shellLog+=data);
  const stopped=new Promise((resolve,reject)=>{shell.once('error',reject);shell.once('exit',(code,signal)=>resolve({code,signal}));});
  const call=(method,args=[])=>run('gdbus',['call','--session','--timeout','3','--dest','dev.phoenix.Cursor','--object-path','/dev/phoenix/Cursor','--method',`dev.phoenix.Cursor.${method}`,'--',...args]);
  let inputApp,inputStopped,blender,blenderStopped;
  try {
    let ready=false,last;
    for(let attempt=0;attempt<100;attempt++) {
      if(shell.exitCode!==null||shell.signalCode!==null)break;
      last=await call('Status');
      if(last.code===0){ready=true;break;}
      await new Promise(resolve=>setTimeout(resolve,200));
    }
    console.log(JSON.stringify({ready,status:last}));
    if(!ready)throw Error('Private GNOME cursor did not become ready');
    const descriptor=JSON.parse(await readFile(join(root,'runtime','phoenix-desktop.json'),'utf8'));
    if(descriptor.scope_key!==process.env.PHOENIX_DESKTOP_SCOPE||descriptor.runtime_dir!==join(root,'runtime')||descriptor.session_bus!==process.env.DBUS_SESSION_BUS_ADDRESS||descriptor.wayland_display!=='phoenix-test')throw Error('Owned compositor descriptor has incorrect session identity');
    if(process.env.PHOENIX_NATIVE_XWAYLAND==='1'&&(!descriptor.display||!descriptor.xauthority))throw Error('Owned compositor did not publish its Xwayland environment');
    console.log(JSON.stringify({descriptor}));
    const statusValue=result=>JSON.parse(result.stdout.slice(result.stdout.indexOf('{'),result.stdout.lastIndexOf('}')+1));
    const initial=statusValue(last);
    for(const [method,args] of [['MoveTo',['400','300','0']],['Click',['left']],['Screenshot',[join(root,'native-cursor.png')]]]) {
      const start=performance.now(),result=await call(method,args);
      console.log(JSON.stringify({method,elapsedMs:performance.now()-start,...result}));
      if(result.code!==0)throw Error(`${method} failed`);
    }
    const finalStatus=await call('Status');
    if(finalStatus.code!==0)throw Error('Final native status unavailable');
    const final=statusValue(finalStatus);
    const png=await readFile(join(root,'native-cursor.png'));
    const checks={
      exactAgentCoordinates:final.agent_cursor.x===400&&final.agent_cursor.y===300,
      userPointerRestored:JSON.stringify(initial.user_pointer)===JSON.stringify(final.user_pointer),
      pngSignature:png.subarray(0,8).equals(Buffer.from([137,80,78,71,13,10,26,10])),
      pngDimensions:png.length>24&&png.readUInt32BE(16)===1280&&png.readUInt32BE(20)===720,
    };
    console.log(JSON.stringify({checks,initial,final}));
    if(!Object.values(checks).every(Boolean))throw Error('Native acceptance check failed');
    const hidden=await call('Hide');
    if(hidden.code!==0)throw Error('Native Hide failed');
    if(process.env.PHOENIX_NATIVE_BLENDER_FILE_DIALOG!=='1') {
    inputApp=spawn('gjs',['-m',resolve('scripts/cursor-native-input-fixture.mjs'),root],{env:{...process.env,WAYLAND_DISPLAY:'phoenix-test',GDK_BACKEND:'wayland'},stdio:['ignore','pipe','pipe']});
    inputApp.stdout.on('data',data=>shellLog+=data);inputApp.stderr.on('data',data=>shellLog+=data);
    inputStopped=new Promise((resolve,reject)=>{inputApp.once('error',reject);inputApp.once('exit',(code,signal)=>resolve({code,signal}));});
    let target;
    for(let attempt=0;attempt<100;attempt++) {
      const listed=await call('ListWindows');
      if(listed.code===0)target=statusValue(listed).windows?.find(window=>window.title==='Phoenix native input acceptance');
      if(target)break;
      if(inputApp.exitCode!==null||inputApp.signalCode!==null)throw Error('Native input application exited');
      await new Promise(resolve=>setTimeout(resolve,100));
    }
    if(!target)throw Error('Native input window not discovered');
    const typed=await call('WindowBatch',[String(target.id),JSON.stringify([{type:'type',text:'Phoenix native typing 42'}])]);
    console.log(JSON.stringify({method:'WindowBatch',...typed}));
    if(typed.code!==0)throw Error('Native window typing failed');
    let textState;
    for(let attempt=0;attempt<30;attempt++) {
      textState=JSON.parse(await readFile(join(root,'input-state.json'),'utf8'));
      if(textState.text==='Phoenix native typing 42')break;
      await new Promise(resolve=>setTimeout(resolve,50));
    }
    console.log(JSON.stringify({nativeTextState:textState}));
    if(textState.text!=='Phoenix native typing 42') {
      console.log(JSON.stringify({diagnosticKey:await call('Key',['a'])}));
      await new Promise(resolve=>setTimeout(resolve,200));
      console.log(JSON.stringify({afterDiagnosticKey:JSON.parse(await readFile(join(root,'input-state.json'),'utf8'))}));
    }
    const captured=await call('CaptureWindow',[String(target.id),join(root,'native-input-window.png')]);
    console.log(JSON.stringify({method:'CaptureWindow',...captured}));
    if(captured.code!==0)throw Error('Native window capture failed');
    if(textState.text!=='Phoenix native typing 42')throw Error('Native application did not receive exact text');
    const unicode='Café — Ελληνικά 日本語 😀 + 42';
    const replaced=await call('WindowBatch',[String(target.id),JSON.stringify([
      {type:'key',combo:'Ctrl+a'}, {type:'type',text:unicode},
      {type:'key',combo:'End'}, {type:'key',combo:'Shift+1'},
    ])]);
    console.log(JSON.stringify({method:'UnicodeAndChords',...replaced}));
    if(replaced.code!==0||!statusValue(replaced).ok)throw Error('Unicode/chord batch rejected');
    for(let attempt=0;attempt<30;attempt++) {
      textState=JSON.parse(await readFile(join(root,'input-state.json'),'utf8'));
      if(textState.text===unicode+'!')break;
      await new Promise(resolve=>setTimeout(resolve,50));
    }
    console.log(JSON.stringify({unicodeTextState:textState,expected:unicode+'!'}));
    const unicodeCapture=await call('CaptureWindow',[String(target.id),join(root,'native-unicode-window.png')]);
    if(unicodeCapture.code!==0||!statusValue(unicodeCapture).ok)throw Error('Unicode window capture failed');
    if(textState.text!==unicode+'!')throw Error('Unicode/chord output differs from requested text');
    if(process.env.PHOENIX_NATIVE_KEYPAD_PROBE==='1') {
      const result=await call('WindowBatch',[String(target.id),JSON.stringify([
        {type:'key',combo:'num1'},{type:'key',combo:'numdecimal'},
      ])]);
      await new Promise(resolve=>setTimeout(resolve,200));
      const state=JSON.parse(await readFile(join(root,'input-state.json'),'utf8'));
      console.log(JSON.stringify({keypadResult:result,keypadState:state}));
      if(!state.keycodes.includes(87)||!state.keycodes.includes(91))throw Error('Native keypad events did not reach the target');
    }
    textState=JSON.parse(await readFile(join(root,'input-state.json'),'utf8'));
    const bounds=textState.button,frame=statusValue(unicodeCapture).frame;
    if(!bounds||bounds.width<=0||bounds.height<=0)throw Error('Native button has no measured allocation');
    const hovered=await call('WindowBatch',[String(target.id),JSON.stringify([
      {type:'move',x:Math.round(frame.x+bounds.x+bounds.width/2),y:Math.round(frame.y+bounds.y+bounds.height/2)},
      {type:'wait',ms:200},
    ])]);
    await new Promise(resolve=>setTimeout(resolve,150));
    const hoverState=JSON.parse(await readFile(join(root,'input-state.json'),'utf8'));
    if(hovered.code!==0||!statusValue(hovered).ok||hoverState.hoverEntries<=textState.hoverEntries||hoverState.clicks!==textState.clicks||hoverState.text!==textState.text)
      throw Error('Native window hover failed or changed the target');
    console.log(JSON.stringify({method:'NativeWindowHover',receipt:statusValue(hovered),before:textState.hoverEntries,after:hoverState.hoverEntries,clicks:hoverState.clicks}));
    const clicked=await call('WindowBatch',[String(target.id),JSON.stringify([{type:'click',
      x:Math.round(frame.x+bounds.x+bounds.width/2),y:Math.round(frame.y+bounds.y+bounds.height/2),
    }])]);
    if(clicked.code!==0||!statusValue(clicked).ok)throw Error('Native click batch failed');
    for(let attempt=0;attempt<20;attempt++) {
      textState=JSON.parse(await readFile(join(root,'input-state.json'),'utf8'));
      if(textState.clicks===1)break;
      await new Promise(resolve=>setTimeout(resolve,50));
    }
    console.log(JSON.stringify({nativeClickCount:textState.clicks,button:bounds,frame}));
    if(textState.clicks!==1)throw Error('Native GTK button did not receive exactly one click');
    const slider=textState.slider,buffer=statusValue(unicodeCapture).buffer;
    if(!slider)throw Error('No native slider allocation');
    if(!slider.windowThumb||!slider.windowTarget)throw Error('Native widget coordinate transform unavailable');
    const baseX=buffer.x+frame.x,baseY=buffer.y+frame.y;
    const positioned=await call('MoveTo',[String(Math.round(baseX+slider.windowThumb.x)),String(Math.round(baseY+slider.windowThumb.y)),'0']);
    if(positioned.code!==0)throw Error('Could not position at native slider thumb');
    const dragged=await call('Drag',[String(Math.round(baseX+slider.windowTarget.x)),String(Math.round(baseY+slider.windowTarget.y)),'200']);
    if(dragged.code!==0)throw Error('Native drag call failed');
    let stableSliderSamples=0,previousSliderValue=null;
    for(let attempt=0;attempt<20;attempt++) {
      textState=JSON.parse(await readFile(join(root,'input-state.json'),'utf8'));
      const value=textState.slider?.value;
      stableSliderSamples=value===previousSliderValue?stableSliderSamples+1:0;
      previousSliderValue=value;
      if(stableSliderSamples>=3&&Math.abs(value-80)<=1)break;
      await new Promise(resolve=>setTimeout(resolve,100));
    }
    console.log(JSON.stringify({sliderBefore:slider,sliderAfter:textState.slider,dragStatus:statusValue(await call('Status')),dragBuffer:buffer,dragFrame:frame}));
    await call('Screenshot',[join(root,'after-slider-drag.png')]);
    if(!Number.isFinite(textState.slider?.value)||Math.abs(textState.slider.value-80)>1||
       Math.min(...textState.slider.values)<19||new Set(textState.slider.values).size<3||stableSliderSamples<3)
      throw Error('Native slider did not settle at 80 within one unit after a centered multi-step drag');
    const currentThumb=textState.slider.windowThumb;
    const windowDrag=await call('WindowBatch',[String(target.id),JSON.stringify([{type:'drag',
      from_x:Math.round(frame.x+currentThumb.x),from_y:Math.round(frame.y+currentThumb.y),
      to_x:Math.round(frame.x+slider.windowThumb.x),to_y:Math.round(frame.y+slider.windowThumb.y),duration_ms:350}])]);
    if(windowDrag.code!==0||!statusValue(windowDrag).ok||statusValue(windowDrag).completed_actions!==1)
      throw Error('Window-targeted native drag failed');
    for(let attempt=0;attempt<20;attempt++) {
      textState=JSON.parse(await readFile(join(root,'input-state.json'),'utf8'));
      if(Math.abs(textState.slider.value-20)<=1)break;
      await new Promise(resolve=>setTimeout(resolve,100));
    }
    if(Math.abs(textState.slider.value-20)>1)throw Error('Window drag did not restore the actual GTK slider to 20');
    console.log(JSON.stringify({windowDrag:true,sliderValue:textState.slider.value,receipt:statusValue(windowDrag)}));
    const scrollTotals=events=>events.reduce((total,event)=>({dx:total.dx+event.dx,dy:total.dy+event.dy}),{dx:0,dy:0});
    const documentBounds=textState.document;
    if(!documentBounds||documentBounds.upper<=documentBounds.page||documentBounds.offset!==0)throw Error('Native document is not a scrollable top-of-page fixture');
    const documentX=Math.round(frame.x+documentBounds.x+documentBounds.width/2);
    const documentY=Math.round(frame.y+documentBounds.y+documentBounds.height/2);
    const overDocument=await call('MoveTo',[String(buffer.x+documentX),String(buffer.y+documentY),'0']);
    if(overDocument.code!==0)throw Error('Could not position over native document');
    let previousDocumentOffset=0;
    for(const [label,request,expected] of [
      ['direct',()=>call('Scroll',['2','3']),{dx:2,dy:3}],
      ['window',()=>call('WindowBatch',[String(target.id),JSON.stringify([{type:'scroll',x:documentX,y:documentY,dx:-1,dy:-2}])]),{dx:1,dy:1}],
      ['direct-negative',()=>call('Scroll',['-1','-1']),{dx:0,dy:0}],
    ]) {
      const result=await request();
      if(result.code!==0||(label==='window'&&!statusValue(result).ok))throw Error(`Native ${label} scroll request failed`);
      let totals;
      for(let attempt=0;attempt<20;attempt++) {
        textState=JSON.parse(await readFile(join(root,'input-state.json'),'utf8'));
        totals=scrollTotals(textState.scrollEvents||[]);
        if(totals.dx===expected.dx&&totals.dy===expected.dy)break;
        await new Promise(resolve=>setTimeout(resolve,100));
      }
      console.log(JSON.stringify({nativeScroll:label,totals,expected,events:textState.scrollEvents}));
      if(totals.dx!==expected.dx||totals.dy!==expected.dy)throw Error(`Native ${label} wheel events did not reach GTK with the requested axes and direction`);
      await new Promise(resolve=>setTimeout(resolve,400));
      textState=JSON.parse(await readFile(join(root,'input-state.json'),'utf8'));
      const offset=textState.document?.offset;
      console.log(JSON.stringify({nativeDocument:label,before:previousDocumentOffset,after:offset}));
      if(!Number.isFinite(offset)||(label==='direct'?offset<=previousDocumentOffset:offset>=previousDocumentOffset))throw Error('Native document viewport did not move in the requested direction');
      previousDocumentOffset=offset;
      const captured=await call('CaptureWindow',[String(target.id),join(root,`native-document-${label}.png`)]);
      if(captured.code!==0||!statusValue(captured).ok)throw Error('Native document capture failed');
    }
    // The application returns keyboard focus to the entry on button action.
    const refocused=await call('WindowBatch',[String(target.id),JSON.stringify([{type:'click',x:Math.round(frame.x+bounds.x+bounds.width/2),y:Math.round(frame.y+bounds.y+bounds.height/2)}])]);
    if(refocused.code!==0||!statusValue(refocused).ok)throw Error('Could not restore fixture text focus');
    let batchFinishedAt;
    const interruptedBatch=call('WindowBatch',[String(target.id),JSON.stringify([
      {type:'key',combo:'Ctrl+a'},{type:'type',text:'safe-prefix'},
      {type:'wait',ms:2000},{type:'type',text:'FORBIDDEN'},
    ])]).then(result=>{batchFinishedAt=performance.now();return result;});
    let prefixSeen=false;
    for(let attempt=0;attempt<30;attempt++) {
      textState=JSON.parse(await readFile(join(root,'input-state.json'),'utf8'));
      if(textState.text==='safe-prefix'){prefixSeen=true;break;}
      await new Promise(resolve=>setTimeout(resolve,50));
    }
    if(!prefixSeen){await interruptedBatch;throw Error('Focus test did not reach its wait boundary');}
    let waitEntered=false;
    for(let attempt=0;attempt<20;attempt++) {
      const batchStatus=await call('Status');
      if(batchStatus.code===0&&statusValue(batchStatus).window_batch?.phase==='waiting'){waitEntered=true;break;}
      await new Promise(resolve=>setTimeout(resolve,20));
    }
    if(!waitEntered){await interruptedBatch;throw Error('Batch never entered the instrumented native wait');}
    const focusRequestedAt=performance.now();
    await writeFile(join(root,'focus-other'),'switch to disposable unrelated target',{flag:'wx',mode:0o600});
    let focusMoved=false;
    for(let attempt=0;attempt<20;attempt++) {
      textState=JSON.parse(await readFile(join(root,'input-state.json'),'utf8'));
      if(textState.otherActive){focusMoved=true;break;}
      await new Promise(resolve=>setTimeout(resolve,50));
    }
    const interrupted=await interruptedBatch;
    await new Promise(resolve=>setTimeout(resolve,150));
    textState=JSON.parse(await readFile(join(root,'input-state.json'),'utf8'));
    const focusRequestToCancellationMs=batchFinishedAt-focusRequestedAt;
    console.log(JSON.stringify({focusMoved,focusRequestToCancellationMs,interrupted,textState}));
    if(!focusMoved||interrupted.code!==0||statusValue(interrupted).ok||textState.text!=='safe-prefix'||textState.otherText!==''||!textState.otherActive)
      throw Error('Focus switch must cancel remaining input without typing into either target or stealing focus back');
    if(focusRequestToCancellationMs>=1500)throw Error('Focus loss held the input slot until the long wait finished');
    const windows=await call('ListWindows');
    const closing=statusValue(windows).windows.find(window=>window.title==='Unrelated native input target');
    if(!closing)throw Error('Missing disposable close target');
    const closeStarted=performance.now();
    const closed=await call('WindowBatch',[String(closing.id),JSON.stringify([
      {type:'key',combo:'alt+f4'},{type:'wait',ms:1200}
    ])]);
    const receipt=statusValue(closed);
    if(closed.code!==0||!receipt.ok||!receipt.focus_changed||receipt.completed_actions!==1||receipt.shortened_waits!==1)
      throw Error('Closing a window after the last input did not return a truthful terminal-transition receipt: '+JSON.stringify(closed));
    let disappeared=false;
    for(let attempt=0;attempt<20;attempt++) {
      const listed=await call('ListWindows');
      if(!statusValue(listed).windows.some(window=>window.id===closing.id)){disappeared=true;break;}
      await new Promise(resolve=>setTimeout(resolve,100));
    }
    if(!disappeared)throw Error('Terminal close receipt did not correspond to an actually closed window');
    console.log(JSON.stringify({terminalClose:true,elapsedMs:performance.now()-closeStarted,receipt,disappeared}));
    }
    if(process.env.PHOENIX_TEST_BLENDER_INPUT==='1') {
      const observePath=join(root,'blender-state.json');
      blender=spawn('/snap/blender/current/blender',['--factory-startup','--disable-autoexec',
        '--window-geometry','0','0','1280','720','--python',resolve('scripts/blender-input-fixture.py'),'--',observePath],
        {env:{...process.env,WAYLAND_DISPLAY:'phoenix-test',DISPLAY:descriptor.display,XAUTHORITY:descriptor.xauthority},stdio:['ignore','pipe','pipe']});
      blender.stdout.on('data',data=>shellLog+=data);blender.stderr.on('data',data=>shellLog+=data);
      blenderStopped=new Promise((resolve,reject)=>{blender.once('error',reject);blender.once('exit',(code,signal)=>resolve({code,signal}));});
      let blenderWindow;
      for(let attempt=0;attempt<150;attempt++) {
        const listed=await call('ListWindows');
        blenderWindow=statusValue(listed).windows.find(window=>window.app?.toLowerCase().includes('blender'));
        if(blenderWindow)break;
        if(blender.exitCode!==null||blender.signalCode!==null)throw Error('Blender fixture exited before readiness');
        await new Promise(resolve=>setTimeout(resolve,100));
      }
      if(!blenderWindow)throw Error('Blender input window not found');
      await call('CaptureWindow',[String(blenderWindow.id),join(root,'blender-startup.png')]);
      const batch=async actions=>{
        const result=await call('WindowBatch',[String(blenderWindow.id),JSON.stringify(actions)]);
        if(result.code!==0||!statusValue(result).ok)throw Error('Blender window batch failed: '+JSON.stringify(result));
      };
      const key=combo=>({type:'key',combo});
      // First launch in this fresh private config has a Quick Setup dialog.
      // Dismiss it through its visible Continue button, then the splash.
      await batch([{type:'click',x:640,y:543},{type:'wait',ms:300}]);
      await batch([{type:'click',x:200,y:300},key('esc'),{type:'click',x:500,y:300},key('a')]);
      let observerReady=false;
      for(let attempt=0;attempt<150;attempt++) {
        try {await readFile(observePath);observerReady=true;break;} catch(error) {if(error.code!=='ENOENT')throw error;}
        await new Promise(resolve=>setTimeout(resolve,100));
      }
      await call('CaptureWindow',[String(blenderWindow.id),join(root,'blender-ready.png')]);
      if(!observerReady)throw Error('Blender observer not ready after startup input; inspect blender-ready.png');
      for(const [axis,value,expected] of [['x','2.55',[2.55,0,0]],['y','1.80',[0,1.8,0]]]) {
        await batch([key('alt+g'),key('g'),key(axis),{type:'type',text:value},key('enter')]);
        let state,cube;
        for(let attempt=0;attempt<30;attempt++) {
          state=JSON.parse(await readFile(observePath,'utf8'));cube=state.objects.find(object=>object.name==='Cube');
          if(cube?.location.every((value,index)=>Math.abs(value-expected[index])<0.001))break;
          await new Promise(resolve=>setTimeout(resolve,100));
        }
        const image=join(root,`blender-numeric-${axis}.png`);
        await call('CaptureWindow',[String(blenderWindow.id),image]);
        console.log(JSON.stringify({blenderNumericAxis:axis,expected,observed:cube?.location,selected:cube?.selected,image}));
        if(!cube?.location.every((value,index)=>Math.abs(value-expected[index])<0.001))throw Error('Blender numeric transform did not reach the requested coordinates');
      }
      // Exercise the actual property-field workflow seen in the group run,
      // independently of keyboard modeling shortcuts. Coordinates come from
      // the fixed 1280x720 factory-startup window capture.
      for(const [route,value] of [['window','3.25'],['screen','4.50']]) {
        if(route==='window') {
          await batch([{type:'double_click',x:1190,y:309},key('ctrl+a'),
            {type:'type',text:value},key('enter')]);
        } else {
          const listed=statusValue(await call('ListWindows'));
          const win=listed.windows.find(window=>window.id===blenderWindow.id);
          if(!win)throw Error('Blender window disappeared before screen field test');
          for(const [method,args] of [
            ['MoveTo',[String(win.x+1190),String(win.y+309),'0']],
            ['DoubleClick',['left']],['Key',['ctrl+a']],['TypeText',[value]],['Key',['enter']],
          ]) {
            const receipt=await call(method,args);
            if(receipt.code!==0)throw Error('Screen property input failed: '+JSON.stringify(receipt));
          }
        }
        let cube;
        for(let attempt=0;attempt<30;attempt++) {
          cube=JSON.parse(await readFile(observePath,'utf8')).objects.find(object=>object.name==='Cube');
          if(Math.abs(cube?.location[0]-Number(value))<0.001)break;
          await new Promise(resolve=>setTimeout(resolve,100));
        }
        const image=join(root,`blender-property-${route}.png`);
        await call('CaptureWindow',[String(blenderWindow.id),image]);
        console.log(JSON.stringify({blenderPropertyRoute:route,expected:Number(value),observed:cube?.location,image}));
        if(Math.abs(cube?.location[0]-Number(value))>=0.001||!cube)throw Error('Blender property field failed through '+route+' input');
      }
      if(process.env.PHOENIX_NATIVE_BLENDER_SELECTION==='1') {
        await batch([{type:'move',x:500,y:300},key('alt+g'),key('tab'),key('1'),key('alt+z'),key('alt+a'),{type:'wait',ms:200}]);
        let before=JSON.parse(await readFile(observePath,'utf8')).edit_meshes;
        if(!before?.length||before.some(mesh=>mesh.selected_vertices!==0))throw Error('Selection fixture did not start with deselected edit vertices');
        await batch([key('b'),{type:'drag',from_x:100,from_y:130,to_x:900,to_y:580,duration_ms:800},{type:'wait',ms:500}]);
        const after=JSON.parse(await readFile(observePath,'utf8')).edit_meshes;
        const image=join(root,'blender-box-selection.png');
        await call('CaptureWindow',[String(blenderWindow.id),image]);
        console.log(JSON.stringify({blenderBoxSelection:{before,after,image}}));
        if(!after?.some(mesh=>mesh.selected_vertices===mesh.vertices))throw Error('Blender box selection did not select every vertex inside the rectangle');
        await batch([{type:'move',x:500,y:300},key('alt+a')]);
        const capture=statusValue(await call('CaptureWindow',[String(blenderWindow.id),join(root,'blender-before-screen-drag.png')]));
        const origin=capture.buffer;
        if(!origin)throw Error('Missing capture origin for screen drag');
        for(const [method,args] of [
          ['MoveTo',[String(origin.x+100),String(origin.y+130),'0']],['Key',['b']],
          ['Drag',[String(origin.x+900),String(origin.y+580),'800']],
        ]) {const result=await call(method,args);if(result.code!==0)throw Error('Blender screen drag failed: '+JSON.stringify(result));}
        await new Promise(resolve=>setTimeout(resolve,500));
        const screenAfter=JSON.parse(await readFile(observePath,'utf8')).edit_meshes;
        const screenImage=join(root,'blender-screen-box-selection.png');
        await call('CaptureWindow',[String(blenderWindow.id),screenImage]);
        console.log(JSON.stringify({blenderScreenBoxSelection:{after:screenAfter,image:screenImage}}));
        if(!screenAfter?.some(mesh=>mesh.selected_vertices>0))throw Error('Screen drag delivered no selected vertices');
        await batch([key('tab')]);
      }
      if(process.env.PHOENIX_NATIVE_BLENDER_FILE_DIALOG==='1') {
        await batch([key('ctrl+shift+s'),{type:'wait',ms:800}]);
        let dialog;
        for(let attempt=0;attempt<40;attempt++) {
          dialog=statusValue(await call('ListWindows')).windows.find(w=>w.app?.toLowerCase().includes('blender')&&w.id!==blenderWindow.id);
          if(dialog)break;
          await new Promise(resolve=>setTimeout(resolve,100));
        }
        if(!dialog)throw Error('Blender Save As dialog did not appear');
        const capture=statusValue(await call('CaptureWindow',[String(dialog.id),join(root,'blender-dialog-before.png')]));
        // Commit an explicit private destination before exercising Save. The
        // default file browser directory may be the user's home.
        const setDirectory=await call('WindowBatch',[String(dialog.id),JSON.stringify([
          {type:'click',x:550,y:50},key('ctrl+a'),{type:'type',text:root+'/'},key('enter'),{type:'wait',ms:300},
        ])]);
        await new Promise(resolve=>setTimeout(resolve,200));
        const destination=JSON.parse(await readFile(observePath,'utf8')).file_browsers;
        if(!statusValue(setDirectory).ok||!destination.some(v=>v.directory===root+'/'))
          throw Error('Private save destination was not verified; refusing Save');
        console.log(JSON.stringify({dialogWindows:statusValue(await call('ListWindows'))}));
        await call('Screenshot',[join(root,'blender-dialog-fullscreen.png')]);
        const before=JSON.parse(await readFile(observePath,'utf8'));
        let receipt;
        if(process.env.PHOENIX_BLENDER_DIALOG_ROUTE==='x11') {
          const env={...process.env,DISPLAY:descriptor.display,XAUTHORITY:descriptor.xauthority};
          for(const args of [
            ['mousemove','--sync',String(dialog.x+280),String(dialog.y+capture.buffer.height-26)],
            ['click','1'],['key','ctrl+a'],['type','--clearmodifiers','native-dialog-check.blend'],['key','Return'],
          ]) {
            const result=await run('xdotool',args,env);
            if(result.code!==0)throw Error('Private X11 input failed: '+JSON.stringify(result));
            await new Promise(resolve=>setTimeout(resolve,250));
          }
          receipt={stdout:JSON.stringify({ok:true,route:'x11'}),code:0};
        } else if(process.env.PHOENIX_BLENDER_DIALOG_ROUTE==='screen') {
          for(const [method,args] of [
            ['MoveTo',[String(dialog.x+400),String(dialog.y+capture.buffer.height-26),'0']],
            ['DoubleClick',['left']],['Key',['ctrl+a']],['TypeText',['native-dialog-check.blend']],['Key',['enter']],
          ]) {
            const result=await call(method,args);
            if(result.code!==0)throw Error('Screen filename operation failed: '+JSON.stringify(result));
            await new Promise(resolve=>setTimeout(resolve,250));
            await call('CaptureWindow',[String(dialog.id),join(root,'blender-dialog-'+method+'.png')]);
          }
          receipt={stdout:JSON.stringify({ok:true,route:'screen'}),code:0};
        } else {
        receipt=await call('WindowBatch',[String(dialog.id),JSON.stringify([
          {type:'move',x:400,y:capture.buffer.height-26},{type:'click',x:400,y:capture.buffer.height-26},{type:'wait',ms:300},{type:'click',x:400,y:capture.buffer.height-26},{type:'wait',ms:300},key('ctrl+a'),
          {type:'type',text:'native-dialog-check.blend'},key('enter'),{type:'wait',ms:300},
        ])]);
        }
        await new Promise(resolve=>setTimeout(resolve,300));
        const after=JSON.parse(await readFile(observePath,'utf8'));
        await call('CaptureWindow',[String(dialog.id),join(root,'blender-dialog-after.png')]);
        console.log(JSON.stringify({method:'BlenderFileDialog',dialog,capture,before:before.file_browsers,after:after.file_browsers,receipt:statusValue(receipt)}));
        if(!after.file_browsers?.some(v=>v.filename==='native-dialog-check.blend'))
          throw Error('Blender filename did not receive the requested text; inspect dialog screenshots');
        const savedPath=join(root,'native-dialog-check.blend');
        let bytes;
        for(let click=0;click<2&&!bytes;click++) {
          const listed=statusValue(await call('ListWindows'));
          if(!listed.windows.some(w=>w.id===dialog.id))break;
          const saved=await call('WindowBatch',[String(dialog.id),JSON.stringify([
            {type:'click',x:980,y:capture.buffer.height-26},{type:'wait',ms:700},
          ])]);
          if(!statusValue(saved).ok)throw Error('Save input failed');
          for(let attempt=0;attempt<15;attempt++) {
            try {bytes=await readFile(savedPath);break;}catch(error){if(error.code!=='ENOENT')throw error;}
            await new Promise(resolve=>setTimeout(resolve,100));
          }
        }
        await call('Screenshot',[join(root,'blender-save-result.png')]);
        if(!bytes?.length)throw Error('No saved blend file; inspect blender-save-result.png');
        console.log(JSON.stringify({method:'BlenderFileDialogSaved',path:savedPath,bytes:bytes.length}));
      }
    }
  } finally {
    if(blender&&blender.exitCode===null&&blender.signalCode===null)blender.kill('SIGTERM');
    if(blenderStopped)await blenderStopped;
    if(inputApp&&inputApp.exitCode===null&&inputApp.signalCode===null)inputApp.kill('SIGTERM');
    if(inputStopped)await inputStopped;
    if(shell.exitCode===null&&shell.signalCode===null)shell.kill('SIGTERM');
    await stopped;
    await writeFile(join(root,'gnome-shell.log'),shellLog,{mode:0o600});
  }
}
