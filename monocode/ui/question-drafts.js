/* Ordinary question answers survive repainting and app reentry. Secure
   credential/approval forms are excluded from this local draft store. */
(function(root){
  'use strict';
  if(root.PhoenixQuestionDrafts)return;
  function key(card){const m=card?._askState;return m&&card.dataset.questionDraftSettled!=='true'&&!m.decision&&!m.login&&!m.teaching&&!m.vault&&!m.pass&&card.dataset.askId?`phoenix-question-draft:${card.dataset.askId}`:'';}
  function save(card){
    const k=key(card),m=card?._askState;if(!k||!card.isConnected)return;
    const customAnswers=[...m.customAnswers],inline=card.querySelector('.approval-inline-custom input');
    if(inline)customAnswers[m.index]=inline.value;
    const alternate=card.querySelector('.approval-custom input');
    const value={index:m.index,answers:m.answers,customAnswers,multiSelections:m.multiSelections.map(s=>[...s]),alternate:alternate?.value||''};
    try{localStorage.setItem(k,JSON.stringify(value));}catch{}
  }
  function restore(card){
    const k=key(card),m=card?._askState;if(!k)return;
    try{
      const v=JSON.parse(localStorage.getItem(k)||'null');if(!v)return;
      if(Number.isInteger(v.index)&&v.index>=0&&v.index<m.questions.length)m.index=v.index;
      m.questions.forEach((q,i)=>{
        if(typeof v.answers?.[i]==='string')m.answers[i]=v.answers[i];
        if(typeof v.customAnswers?.[i]==='string')m.customAnswers[i]=v.customAnswers[i];
        if(Array.isArray(v.multiSelections?.[i]))m.multiSelections[i]=new Set(v.multiSelections[i].filter(x=>q.options.includes(x)));
      });
      const alternate=card.querySelector('.approval-custom input');if(alternate&&typeof v.alternate==='string')alternate.value=v.alternate;
    }catch{}
  }
  function discard(card){const k=key(card);if(k){card.dataset.questionDraftSettled='true';try{localStorage.removeItem(k);}catch{}}}
  function persistAll(){document.querySelectorAll('#approvalStack .approval-card').forEach(save);}
  function track(event){const card=event.target.closest?.('#approvalStack .approval-card');if(key(card))queueMicrotask(()=>save(card));}
  for(const event of ['input','change','click'])document.addEventListener(event,track,true);
  addEventListener('beforeunload',persistAll);addEventListener('pagehide',persistAll);
  root.PhoenixQuestionDrafts=Object.freeze({save,restore,discard,persistAll});
})(globalThis);
