/* Only the separate connected preview. All operations cross guarded native IPC. */
(function(root){'use strict';
 if(!root.PhoenixIsolatedBackend?.enabled)return;
 const api=root.PhoenixIsolatedBackend;root.__PHOENIX_ISOLATED_BACKEND__={enabled:true,executionBlocked:!api.current,current:api.current,userInitiatedTurns:api.current};
 if(api.current)root.__PHOENIX_CURRENT_BACKEND__={enabled:true,approvalPending:true};
 root.__PHOENIX_CHROMIUM_SHELL__={enabled:true,engine:'isolated-native-electron'};
 root.__TAURI__={core:{invoke:(command,args)=>api.invoke(command,args||{})},event:{listen:async(name,callback)=>api.nativeListen(name,callback)}};
 let serial=0;
 class PhoenixSocket extends EventTarget{
  static CONNECTING=0;static OPEN=1;static CLOSING=2;static CLOSED=3;
  constructor(url){super();this.url=String(url);this.readyState=0;this.id=String(++serial);this.binaryType='blob';this.remove=api.listen(this.id,(type,value)=>{if(this.readyState===3)return;if(type==='message')this.emit(type,new MessageEvent('message',{data:value}));if(type==='error')this.emit(type,new Event('error'));if(type==='close'){this.readyState=3;this.remove?.();this.emit(type,new CloseEvent('close',{wasClean:true}));}});api.open(this.id).then(()=>{if(this.readyState!==0)return;this.readyState=1;this.emit('open',new Event('open'))}).catch(()=>{this.emit('error',new Event('error'));this.close()});}
  emit(type,event){this.dispatchEvent(event);this['on'+type]?.(event)}
  send(data){if(this.readyState!==1)throw new DOMException('Socket is not open','InvalidStateError');api.send(this.id,String(data)).catch(error=>{this.emit('message',new MessageEvent('message',{data:JSON.stringify({Error:{message:String(error.message||error)}})}));this.close()});}
  close(){if(this.readyState===3)return;this.readyState=3;api.close(this.id);this.remove?.();this.emit('close',new CloseEvent('close',{wasClean:true}));}
 }
 root.WebSocket=PhoenixSocket;
 document.documentElement.dataset.backend=api.current?'current':'isolated';
})(globalThis);
