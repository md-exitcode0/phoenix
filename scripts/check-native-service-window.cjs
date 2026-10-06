'use strict';
// Execute the actual native window constructor with inert Electron objects.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const source=fs.readFileSync(path.join(__dirname,'../canvas-app/chromium-shell/main.cjs'),'utf8');
const constructor=source.slice(source.indexOf('function createMainWindow() {'),source.indexOf('// Expected native failures'));
const reveal=source.slice(source.indexOf('function revealMainWindow('),source.indexOf('app.setName('));
let passed=0;
for(const services of [false,true]){
 let instance;
 class Window{
  constructor(options){this.options=options;this.shown=0;this.focused=0;instance=this;this.webContents={on(){},once(){},setWindowOpenHandler(){},getZoomFactor(){return 1;},setZoomFactor(){}};}
  setMenu(){} on(){} once(){} loadFile(file){this.file=file;} loadURL(url){this.url=url;} isDestroyed(){return false;} isMinimized(){return false;} isVisible(){return this.shown>0;} isFocused(){return this.focused>0;} show(){this.shown++;} focus(){this.focused++;}
 }
 const context=vm.createContext({BrowserWindow:Window,SELFTEST:false,SERVICES_ONLY:services,CONVERSATION_URL:null,PRELOAD:'/app/preload',UI_ROOT:'/app/ui',__dirname:'/app/shell',PHOENIX_HOME:'/fixture',path,mainWindow:null,zoomControl:{},fs:{readFileSync(){throw Error('no saved zoom');}},surfaces:new Map(),SHOW_DELAY_MS:0,setTimeout(){},bootLog(){}});
 vm.runInContext(constructor+reveal+';createMainWindow();revealMainWindow("fixture");',context);
 assert.equal(instance.options.show,!services);passed++;
 assert.equal(instance.options.skipTaskbar,services);passed++;
 assert.equal(instance.file,services?'/app/ui/native-services.html':'/app/ui/index.html');passed++;
 assert.equal(instance.shown,services?0:1);passed++;
 assert.equal(instance.focused,services?0:1);passed++;
 assert.equal(instance.options.webPreferences.sandbox,true);passed++;
 assert.equal(instance.options.webPreferences.contextIsolation,true);passed++;
 assert.equal(instance.options.webPreferences.nodeIntegration,false);passed++;
}
const html=fs.readFileSync(path.join(__dirname,'../canvas-app/ui/native-services.html'),'utf8');
const tabConstructor=source.slice(source.indexOf('function createSurfaceTab('),source.indexOf('  view.setBackgroundColor(surfaceBackground);'))+'  return view;\n}';
for(const services of [false,true]){
 const context=vm.createContext({SERVICES_ONLY:services,WebContentsView:class{constructor(options){this.options=options;}}});
 vm.runInContext(tabConstructor+';globalThis.view=createSurfaceTab({profileOwner:"agent-phoenix"});',context);
 assert.equal(context.view.options.webPreferences.offscreen,undefined);passed++;
 assert.equal(context.view.options.webPreferences.partition,'persist:phoenix-agent-phoenix');passed++;
 assert.equal(context.view.options.webPreferences.sandbox,true);passed++;
}
assert.equal(/<script|<iframe|<img|<link/i.test(html),false);passed++;
assert.match(html,/default-src 'none'/);passed++;
console.log(JSON.stringify({passed,scope:'Actual constructor/reveal function with inert Electron; static service page has no conversation/assets/network scripts.'}));
