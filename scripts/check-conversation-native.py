import gi, pathlib, tempfile, os, json, shutil
# Run with xvfb-run -a python3 scripts/check-conversation-native.py.
# Isolated native WebKit preview; no gateway connection or model calls.
from shutil import copy2
gi.require_version('Gtk','3.0');gi.require_version('WebKit2','4.1')
from gi.repository import Gtk, WebKit2, GLib, Gdk
root=pathlib.Path.cwd();tmp=pathlib.Path(tempfile.mkdtemp(prefix='phoenix-native-ui-'));out=root/('artifacts/conversation-redesign'+os.environ.get('PHOENIX_TEST_SUFFIX',''));out.mkdir(parents=True,exist_ok=True)
for p in (root/'canvas-app/ui').iterdir():
 if p.name=='conversation.js':continue
 (tmp/p.name).symlink_to(p)
(out/'approval-check.txt').unlink(missing_ok=True)
s=(root/'canvas-app/ui/conversation.js').read_text();s=s.replace('  bind(); renderVoiceState(); autosize();','  window.__timelineTest={state,renderHandoff,renderIncomingAgentTalk,renderCommentary,renderAnswer,renderGroupMemberStatus,renderReturn,historyVisibleInConversation,normalizeGroupOperationalAgent,toggleGroupTurnWork,renderApproval,renderHistory};\n  bind(); renderVoiceState(); autosize();');(tmp/'conversation.js').write_text(s)
w=Gtk.Window();w.set_default_size(int(os.environ.get("PHOENIX_TEST_WIDTH","1440")),1000);view=WebKit2.WebView();view.get_settings().set_allow_file_access_from_file_urls(True);w.add(view);w.show_all()
script=r'''(()=>{const t=window.__timelineTest;if(!t)throw Error('Timeline not loaded');const s=t.state;s.item={kind:'group',id:'build-group',name:'Build Group'};s.activeTurnId='visual-turn';s.renderingTurnId='';s.painting=false;s.working=true;s.turnStatus=null;s.replayWorkCluster=null;s.displayRows=[];document.getElementById('conversationFeed').innerHTML='';
t.renderCommentary('phoenix','I’ll ask Theo to verify the source dates, then use the results to update the recommendation.');
t.renderHandoff({from:'phoenix',to:'researcher',subject:'Verify publication dates and current availability',handoff_id:'sample-handoff'});
t.renderCommentary('researcher','The page is still open, but its original publication date is months old. I’m checking for a recent owner confirmation.');
t.renderReturn({agent:'researcher',reply_to:'sample-handoff',body:'Availability is unconfirmed. The listing is from May.',ok:true});
t.renderIncomingAgentTalk({from:'researcher',reply_to:'sample-handoff',body:'The listing dates to **May 28**. I found no current confirmation that it is available. It should remain an unqualified lead.'});
t.renderCommentary('phoenix','Theo’s check ruled out the earlier recommendation. I’m comparing the remaining candidates against current availability evidence.');
t.renderGroupMemberStatus({turn_id:'visual-turn',agent_id:'phoenix',state:'working',detail:'Execution lane acquired'});
const feed=document.getElementById('conversationFeed');feed.scrollTop=0;
const peer=feed.querySelector('.peer-message');if(peer.classList.contains('user-message'))throw Error('Peer on user side');if(feed.querySelectorAll(':scope>.handoff-chain').length!==1)throw Error('Handoff missing');if([...feed.querySelectorAll('.group-execution-strip')].some(n=>!n.hidden))throw Error('Started card visible');
const names=[...feed.querySelectorAll('.work-cluster')].map(n=>n.dataset.agent);if(names.join(',')!=='phoenix,researcher,phoenix')throw Error('Out of order: '+names);
s.item={kind:'agent',id:'researcher'};if(t.historyVisibleInConversation({role:'tool',agent:'phoenix',tool:'read'}))throw Error('Foreign work leaked');s.item={kind:'group',id:'build-group'};s.activeGroupAgentIds=['researcher'];if(t.normalizeGroupOperationalAgent({kind:'tool',agent:'phoenix'}).agent!=='phoenix')throw Error('Speaker overwritten');
const oldCount=feed.querySelectorAll('.handoff-chain').length;
s.painting=true;t.renderHistory({role:'talk',from:'phoenix',to:'scribe',subject:'Check wording',text:'Keep the exact meaning',reply_expected:true,handoff_id:'history-handoff'});
if(feed.querySelectorAll('.handoff-chain').length!==oldCount+1)throw Error('Replay hid a handoff');
t.renderHistory({role:'talk',from:'scribe',to:'phoenix',subject:'Wording checked',text:'The wording preserves the meaning.',reply_expected:false,reply_to:'history-handoff'});
if(feed.querySelectorAll('.peer-message').length!==2)throw Error('Replay hid return');
s.painting=false;
const clusters=[...feed.querySelectorAll('.work-cluster')];const was=clusters[0].classList.contains('group-work-expanded');t.toggleGroupTurnWork(clusters[1]);if(clusters[0].classList.contains('group-work-expanded')!==was)throw Error('Disclosure toggled other authors');
return JSON.stringify({ok:true,names,peerSide:'assistant',handoffs:2,replay:true,isolatedDisclosure:true});})()'''
def finish(v,res,*_):
 try:
  result=v.run_javascript_finish(res).get_js_value().to_string();(out/'checks.json').write_text(result);print(result,flush=True)
  GLib.timeout_add(600,capture)
 except Exception as e:print('FAILED',e,flush=True);Gtk.main_quit();raise

def capture():
 pix=Gdk.pixbuf_get_from_window(w.get_window(),0,0,w.get_allocated_width(),w.get_allocated_height());pix.savev(str(out/'timeline.png'),'png',[],[]);print(out/'timeline.png',flush=True);
 view.run_javascript(approval_script,None,approval_checked,None);return False
approval_script=r'''(()=>{const t=window.__timelineTest;document.getElementById('approvalStack').innerHTML='';const question='Proposed message:\n\nHello, could you confirm which scope is available?\n\nDo you approve posting this exact message?';t.renderApproval({id:'fixture-approval',agent:'phoenix',questions:[{question,options:['Approve posting exactly as written','Not now']}],approval:{action:'teach_workflow',approved_option:'Approve posting exactly as written',details:{workflow_goal:'Post the message'}}});const card=document.querySelector('.approval-card');if(card.dataset.teaching!=='false')throw Error('Posting approval became teaching');if(!document.querySelector('.decision-request').textContent.includes(question.split('\n')[2]))throw Error('Draft hidden in conversation');if(!card.textContent.includes('could you confirm'))throw Error('Draft hidden in card');return 'approval text visible; teaching disabled';})()'''
def approval_checked(v,res,*_):
 try:print(v.run_javascript_finish(res).get_js_value().to_string(),flush=True);GLib.timeout_add(600,approval_capture)
 except Exception as e:print('FAILED',e,flush=True);Gtk.main_quit()
def approval_capture():
 pix=Gdk.pixbuf_get_from_window(w.get_window(),0,0,w.get_allocated_width(),w.get_allocated_height());pix.savev(str(out/'approval.png'),'png',[],[]);(out/'approval-check.txt').write_text('PASS');Gtk.main_quit();return False
def run():view.run_javascript(script,None,finish,None);return False
view.load_uri((tmp/'index.html').as_uri()+'?shot=group');GLib.timeout_add(2500,run);GLib.timeout_add_seconds(20,lambda:Gtk.main_quit());Gtk.main();shutil.rmtree(tmp)
if not (out/"approval-check.txt").exists():raise SystemExit("Native preview check failed")
