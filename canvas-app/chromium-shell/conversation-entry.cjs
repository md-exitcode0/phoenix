'use strict';
// An optional local conversation frontend, on the ordinary native Phoenix
// window/preload/browser surfaces. This never selects a different backend.
function validate(raw){
 if(!raw)return null;
 const url=new URL(raw);
 if(url.protocol!=='http:'||url.hostname!=='127.0.0.1'||url.port!=='47845'||url.username||url.password||url.pathname!=='/'||url.hash||url.search!=='?skin=monocode&chromium=1')throw Error('The conversation frontend must use the owned local Phoenix origin');
 return url.href;
}
module.exports={validate};
