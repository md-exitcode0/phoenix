'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const {validate}=require('../canvas-app/chromium-shell/conversation-entry.cjs');
const url='http://127.0.0.1:47845/?skin=monocode&chromium=1';
let passed=0;
assert.equal(validate(undefined),null);passed++;
assert.equal(validate(url),url);passed++;
for(const raw of ['https://example.com/',url.replace('127.0.0.1','localhost'),url.replace('47845','47846'),url+'#other',url.replace('/?', '/other?'),url.replace('127.0.0.1','user:pass@127.0.0.1'),url+'&backend=private']){assert.throws(()=>validate(raw));passed++;}
const source=fs.readFileSync(path.join(__dirname,'../canvas-app/chromium-shell/main.cjs'),'utf8');
const constructor=source.slice(source.indexOf('function createMainWindow() {'),source.indexOf('// Expected native failures'));
let window;
class Window{constructor(options){this.options=options;this.webContents={on(){},once(){},setWindowOpenHandler(){}};window=this;}setMenu(){}on(){}once(){}loadURL(value){this.url=value;}loadFile(){throw Error('Native conversation must keep its existing preload/window');}}
const context=vm.createContext({BrowserWindow:Window,SELFTEST:false,SERVICES_ONLY:false,CONVERSATION_URL:url,PRELOAD:'/original/preload.cjs',UI_ROOT:'/original/ui',__dirname:'/original/shell',PHOENIX_HOME:'/fixture',path,mainWindow:null,zoomControl:{},surfaces:new Map(),SHOW_DELAY_MS:0,setTimeout(){}});
vm.runInContext(constructor+';createMainWindow();',context);
assert.equal(window.url,url);passed++;
assert.equal(window.options.webPreferences.preload,'/original/preload.cjs');passed++;
assert.equal(window.options.show,true);passed++;
assert.equal(window.options.webPreferences.sandbox,true);passed++;
assert.equal(window.options.webPreferences.contextIsolation,true);passed++;
assert.equal(window.options.webPreferences.nodeIntegration,false);passed++;
console.log(JSON.stringify({passed,scope:'Actual native constructor and local frontend validator, with inert Electron. No app/backend/profile started.'}));
