// Conversation header motion helpers. The bottom-of-thread working orb was
// replaced by the status pill that conversation.js owns (#conversationThinking,
// driven by status-pill.js): one indicator, one source of truth.
const header = document.getElementById('conversationHeader');
const root = document.documentElement;
globalThis.PhoenixThinkingOrb = {diagnostics:()=>({state:'status-pill', running:false, destroyed:false})};

const mode=document.createElement('button');
mode.id='conversationDetailToggle';mode.type='button';
header.querySelector('.header-spacer').before(mode);
function syncMode(){const chat=root.dataset.conversationView==='chat';mode.textContent=chat?'Chat only':'Tool activity';mode.setAttribute('aria-pressed',String(!chat));mode.title=chat?'Show tool calls':'Show messages without tool calls';mode.setAttribute('aria-label',chat?'Chat only. Show tool activity':'Tool activity. Switch to chat only');}
mode.addEventListener('click',()=>{const value=root.dataset.conversationView==='chat'?'compact':'chat';globalThis.PhoenixSettings.setConversationView(value);});
addEventListener('phoenix:visual-prefs-changed',syncMode);syncMode();
addEventListener('pagehide',()=>{removeEventListener('phoenix:visual-prefs-changed',syncMode);},{once:true});
