// Keep the completed reply primary, without discarding deliberate earlier updates.
(() => {
  const scope = node => ({turn: node?.dataset.turnId, speaker: node?.dataset.speaker || node?.dataset.agentId});
  const matches = (a, b) => {
    const left = scope(a), right = scope(b);
    return Boolean(left.turn && left.speaker && left.turn === right.turn && left.speaker === right.speaker);
  };
  function finalize(answer) {
    const feed = answer?.parentElement, content = answer?.querySelector('.message-content');
    if (!feed || !content || !scope(answer).turn || !scope(answer).speaker) return;
    answer.dataset.publicReply = 'true';
    const updates = [...feed.children].filter(node => node.matches('.commentary-message') && node.dataset.publicReply !== 'true' && matches(node, answer));
    if (!updates.length) return;
    let disclosure = content.querySelector(':scope > .answer-earlier-updates');
    if (!disclosure) {
      disclosure = document.createElement('details');
      disclosure.className = 'answer-earlier-updates';
      const summary = document.createElement('summary');
      summary.textContent = 'Earlier updates';
      const body = document.createElement('div');
      body.className = 'answer-earlier-updates-body';
      disclosure.append(summary, body);
      content.insertBefore(disclosure, content.querySelector(':scope > footer'));
    }
    const body = disclosure.querySelector('.answer-earlier-updates-body');
    body.replaceChildren(...updates.map(node => node.querySelector('.markdown')?.cloneNode(true)).filter(Boolean));
    updates.forEach(node => node.classList.add('superseded-update'));
  }
  function acceptUpdate(update) {
    if (update?.dataset.publicReply === 'true') return;
    // A delayed history receipt can arrive after the completed reply.
    const answer = [...(update?.parentElement?.children || [])].reverse().find(node => node.dataset.publicReply === 'true' && matches(node, update));
    if (answer) finalize(answer);
  }
  window.PhoenixConversationUpdates = Object.freeze({finalize, acceptUpdate});
})();
