'use strict';
const $ = (selector) => document.querySelector(selector);
const root = document.documentElement;
const tasks = {
  build:{brief:'Build a clear landing page for my product. Show what it does, make the mobile version work, and test the main actions.',team:['Iris','Theo','Leo'],outcome:'A website you can open and use.',explanation:'Iris handles the interface. Theo can research assets and facts. Leo can check the code and interactions. You can inspect the files and the page as the work develops.'},
  research:{brief:'Compare these three tools for my small business. Use current sources, explain the tradeoffs, and tell me what I still need to check.',team:['Theo','Vera','Remy'],outcome:'A comparison you can make a decision with.',explanation:'Theo gathers sources. Vera can examine costs. Remy can challenge the gaps. The useful output is a clear comparison with evidence, not a wall of links or an unsupported recommendation.'},
  code:{brief:'Find why this test fails, fix the cause, and run the relevant checks. Show me what changed before I commit anything.',team:['Leo','Remy'],outcome:'Changes you can inspect in your own project.',explanation:'Leo works with the repository and terminal. Remy can review the result. You keep the files, tests and changes together; a commit or deployment is a separate decision.'},
  files:{brief:'Read these project notes and turn them into a short plan. Keep the decisions, flag contradictions, and list the next actions with their owners.',team:['Elena','Maya'],outcome:'A useful plan built from your source material.',explanation:'Elena works with documents and knowledge. Maya can help organize actions and owners. The original material remains the basis for the result, with uncertainty called out rather than guessed away.'}
};
const people={
  phoenix:{role:'Your point person',text:'Phoenix helps break down a task, bring in the right coworkers, and give you one clear response.'},
  iris:{role:'Design & frontend',text:'Iris works on interfaces, layouts and frontend code. Her job includes making the result clear, useful and verified on a real screen.'},
  leo:{role:'Code & engineering',text:'Leo works in your repository and terminal: investigating problems, changing code, running checks and preparing work you can review.'},
  theo:{role:'Research & sources',text:'Theo gathers current information, compares sources and helps separate evidence from assumptions.'},
  elena:{role:'Documents & knowledge',text:'Elena turns source material into organized documents, reports and reusable knowledge while keeping important context intact.'},
  remy:{role:'Reliability & review',text:'Remy investigates failures, checks assumptions and reviews whether the work behaves as intended.'}
};
let selectedTask='build',previewTheme='light',dialogOpener=null;
function selectTask(name){
  if(!Object.hasOwn(tasks,name)) return;
  selectedTask=name;const task=tasks[name];
  document.querySelectorAll('[data-task]').forEach(button=>{const active=button.dataset.task===name;button.setAttribute('aria-selected',String(active));button.tabIndex=active?0:-1;});
  $('#task-panel').setAttribute('aria-labelledby','tab-'+name);
  $('#task-brief').textContent=task.brief;$('#task-outcome').textContent=task.outcome;$('#task-explanation').textContent=task.explanation;
  $('#task-team').replaceChildren(...task.team.map(name=>{const chip=document.createElement('span');chip.className='mini-person';const mark=document.createElement('i');mark.setAttribute('aria-hidden','true');mark.textContent=name[0];chip.append(mark,document.createTextNode(name));return chip;}));
  $('#copy-status').textContent='';$('#copy-brief').firstChild.textContent='Copy this brief ';
}
document.querySelectorAll('[data-task]').forEach(button=>{
  button.addEventListener('click',()=>selectTask(button.dataset.task));
  button.addEventListener('keydown',event=>{const tabs=[...document.querySelectorAll('[data-task]')];let index=tabs.indexOf(button);if(event.key==='ArrowRight'||event.key==='ArrowDown')index=(index+1)%tabs.length;else if(event.key==='ArrowLeft'||event.key==='ArrowUp')index=(index-1+tabs.length)%tabs.length;else if(event.key==='Home')index=0;else if(event.key==='End')index=tabs.length-1;else return;event.preventDefault();selectTask(tabs[index].dataset.task);tabs[index].focus();});
});
$('#copy-brief').addEventListener('click',async()=>{
  const name=selectedTask;
  try{if(!navigator.clipboard?.writeText)throw Error('Clipboard unavailable');await navigator.clipboard.writeText(tasks[name].brief);if(selectedTask===name)$('#copy-status').textContent='Brief copied.';}
  catch{if(selectedTask!==name)return;const range=document.createRange();range.selectNodeContents($('#task-brief'));getSelection()?.removeAllRanges();getSelection()?.addRange(range);$('#copy-status').textContent='Text selected. Copy it with your keyboard.';}
});
document.querySelectorAll('[data-person]').forEach(button=>button.addEventListener('click',()=>{
  const name=button.dataset.person;if(!Object.hasOwn(people,name))return;
  document.querySelectorAll('[data-person]').forEach(p=>{const active=p===button;p.setAttribute('aria-pressed',String(active));p.classList.toggle('selected',active);});
  $('#person-role').textContent=people[name].role;$('#person-description').textContent=people[name].text;
}));
const storage={get(key){try{return localStorage.getItem(key);}catch{return null;}},set(key,value){try{localStorage.setItem(key,value);}catch{/* Theme remains usable for this visit. */}}};
function setTheme(theme){root.dataset.theme=theme;$('#theme-toggle').setAttribute('aria-label',`Switch to ${theme==='dark'?'light':'dark'} theme`);$('meta[name="theme-color"]').content=theme==='dark'?'#191d1c':'#f5f2ed';}
const saved=storage.get('phoenix-site-theme');setTheme(saved==='dark'?'dark':'light');
$('#theme-toggle').addEventListener('click',()=>{const next=root.dataset.theme==='dark'?'light':'dark';setTheme(next);storage.set('phoenix-site-theme',next);});
document.querySelectorAll('[data-preview-theme]').forEach(button=>button.addEventListener('click',()=>{
  previewTheme=button.dataset.previewTheme;$('#product-image').src=`assets/phoenix-team-${previewTheme}.png`;
  document.querySelectorAll('[data-preview-theme]').forEach(b=>b.setAttribute('aria-pressed',String(b===button)));
}));
function closeMenu(){const toggle=$('#menu-toggle');toggle.setAttribute('aria-expanded','false');$('#site-nav').classList.remove('open');}
$('#menu-toggle').addEventListener('click',()=>{const open=$('#menu-toggle').getAttribute('aria-expanded')!=='true';$('#menu-toggle').setAttribute('aria-expanded',String(open));$('#site-nav').classList.toggle('open',open);});
$('#site-nav').querySelectorAll('a').forEach(a=>a.addEventListener('click',closeMenu));
addEventListener('resize',()=>{if(innerWidth>760)closeMenu();});
addEventListener('keydown',e=>{if(e.key==='Escape'&&$('#menu-toggle').getAttribute('aria-expanded')==='true'){closeMenu();$('#menu-toggle').focus();}});
function openDialog(dialog,opener){dialogOpener=opener;dialog.showModal();dialog.querySelector('[data-close]').focus();}
document.querySelectorAll('[data-release]').forEach(button=>button.addEventListener('click',()=>openDialog($('#release-dialog'),button)));
$('#release-tour').addEventListener('click',()=>$('#release-dialog').close());
$('#open-product-image').addEventListener('click',event=>{ $('#enlarged-image').src=`assets/phoenix-team-${previewTheme}.png`;openDialog($('#image-dialog'),event.currentTarget);});
document.querySelectorAll('dialog').forEach(dialog=>{
  dialog.querySelector('[data-close]').addEventListener('click',()=>dialog.close());
  dialog.addEventListener('click',event=>{if(event.target!==dialog)return;const r=dialog.getBoundingClientRect();if(event.clientX<r.left||event.clientX>r.right||event.clientY<r.top||event.clientY>r.bottom)dialog.close();});
  dialog.addEventListener('close',()=>{dialogOpener?.focus({preventScroll:true});dialogOpener=null;});
});
selectTask('build');
// Native, one-time section entrances. The first screen and restored views never hide.
const motion=matchMedia('(prefers-reduced-motion: reduce)');
const activeAnimations=new Set();
const observer=typeof IntersectionObserver==='function'?new IntersectionObserver(entries=>{
  for(const entry of entries){if(!entry.isIntersecting)continue;observer.unobserve(entry.target);if(motion.matches||document.hidden||!entry.target.animate)continue;const animation=entry.target.animate([{opacity:.25,transform:'translateY(16px)'},{opacity:1,transform:'none'}],{duration:550,easing:'cubic-bezier(.22,1,.36,1)'});activeAnimations.add(animation);animation.finished.then(()=>activeAnimations.delete(animation)).catch(()=>activeAnimations.delete(animation));}
},{threshold:.08}):null;
document.querySelectorAll('.section-heading,.team-intro,.control-copy,.questions>div:first-child').forEach(el=>{if(el.getBoundingClientRect().top>innerHeight)observer?.observe(el);});
motion.addEventListener('change',()=>{if(motion.matches){observer?.disconnect();for(const animation of activeAnimations)animation.cancel();activeAnimations.clear();}});
