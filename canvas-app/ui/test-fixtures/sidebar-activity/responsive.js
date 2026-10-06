// Geometry and interaction checks against the actual preview shell. No gateway.
(() => {
  const $ = id => document.getElementById(id);
  const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
  const visible = node => Boolean(node && node.getClientRects().length && getComputedStyle(node).visibility !== 'hidden');
  const rect = node => {
    const r = node.getBoundingClientRect();
    return { left: r.left, right: r.right, top: r.top, bottom: r.bottom, width: r.width, height: r.height };
  };
  const inside = (node, parent) => {
    const a = rect(node), b = rect(parent);
    return a.left >= b.left - 1 && a.right <= b.right + 1;
  };
  const ensure = (value, message) => { if (!value) throw new Error(message); };
  const geometry = () => ['appShell', 'conversationStage', 'conversationHeader', 'conversationBody', 'conversationFeed', 'composerZone', 'composer'].map(id => {
    const n = $(id), s = getComputedStyle(n);
    return { id, ...rect(n), clientWidth: n.clientWidth, scrollWidth: n.scrollWidth, minWidth: s.minWidth, gridColumns: s.gridTemplateColumns, overflowX: s.overflowX };
  });
  const headerIds = ['stageMore', 'groupRosterButton', 'historyButton', 'teachAgentButton', 'stageInspectButton', 'terminalToggle', 'stageSidebarButton'];
  const composerIds = ['modeButton', 'modelButton', 'composerOptionsButton', 'permissionButton', 'voiceButton', 'contextButton', 'sendButton'];

  window.prepareResponsiveFixture = async () => {
    ensure(window.PhoenixUI && !window.PhoenixUI.TAURI, 'Use the isolated browser preview');
    document.body.classList.remove('sidebar-collapsed');
    document.documentElement.dataset.motion = 'minimal';
    await window.runSidebarAcceptance();
    await document.fonts.ready;
    await pause(400);
    $('conversationFeed').scrollTop = 0;
    window.PhoenixUI.closeLayers();
    return geometry();
  };

  window.checkResponsiveFixture = async (stress = false) => {
    const checks = [];
    const check = (name, run) => {
      try { checks.push({ name, ok: true, detail: run() }); }
      catch (error) { checks.push({ name, ok: false, error: String(error.message || error) }); }
    };
    const stage = $('conversationStage'), feed = $('conversationFeed');
    check('conversation grid children fit the available pane', () => {
      for (const id of ['conversationHeader', 'conversationBody', 'conversationFeed', 'composerZone', 'composer']) {
        ensure(inside($(id), stage), `${id}: ${JSON.stringify(rect($(id)))} outside ${JSON.stringify(rect(stage))}`);
      }
    });
    check('message and team block edges remain readable', () => {
      for (const node of feed.querySelectorAll('.message-row, .message-content, .user-bubble, .team-work-block')) {
        ensure(inside(node, feed) && inside(node, stage), `${node.className}: right ${rect(node).right} > pane ${rect(stage).right}`);
      }
      ensure(feed.scrollWidth <= feed.clientWidth + 1, `Feed clips ${feed.scrollWidth - feed.clientWidth}px`);
    });
    check('all header actions have visible non-overlapping targets', () => {
      const nodes = headerIds.map($);
      for (const node of nodes) {
        ensure(visible(node), `${node?.id} hidden`);
        const r = rect(node);
        ensure(inside(node, stage) && r.width >= 24 && r.height >= 24, `${node.id}: ${JSON.stringify(r)}`);
        ensure(r.top >= rect(stage).top && r.bottom <= rect($('conversationBody')).top + 1, `${node.id} overlaps the conversation`);
        const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
        ensure(hit && node.contains(hit), `${node.id} is obstructed`);
      }
      for (let i = 0; i < nodes.length; i++) for (let j = i + 1; j < nodes.length; j++) {
        const a = rect(nodes[i]), b = rect(nodes[j]);
        ensure(Math.min(a.right, b.right) - Math.max(a.left, b.left) < 1 || Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top) < 1, `${nodes[i].id} overlaps ${nodes[j].id}`);
      }
      return nodes.map(node => ({ id: node.id, ...rect(node) }));
    });
    check('composer input and primary controls remain reachable', () => {
      for (const id of ['composerInput', ...composerIds]) {
        const node = $(id), r = rect(node);
        ensure(visible(node) && inside(node, stage), `${id} hidden or clipped`);
        ensure(r.top >= rect($('conversationBody')).top && r.bottom <= innerHeight, `${id} outside the visible pane`);
        const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
        ensure(hit && node.contains(hit), `${id} is obstructed`);
      }
      $('composerInput').focus({ preventScroll: true });
      ensure(document.activeElement === $('composerInput'), 'Composer cannot receive focus');
    });
    check('history and composer options remain usable', () => {
      try {
        if ($('historyButton').getAttribute('aria-expanded') !== 'true') $('historyButton').click();
        ensure($('historyButton').getAttribute('aria-expanded') === 'true' && visible($('conversationPromptRail')), 'History did not open');
        ensure(inside($('conversationPromptRail'), stage), 'History clips outside the pane');
        if ($('composerOptions').hidden) $('composerOptionsButton').click();
        ensure(!$('composerOptions').hidden && inside($('composerOptions'), stage), 'Composer options did not fit');
        for (const id of ['contextPickerButton', 'reasoningButton']) ensure(visible($(id)) && inside($(id), stage), `${id} unavailable`);
      } finally {
        if ($('historyButton').getAttribute('aria-expanded') === 'true') $('historyButton').click();
        if (!$('composerOptions').hidden) $('composerOptionsButton').click();
      }
    });
    if (stress) {
      const message = feed.querySelector('.group-message .markdown');
      ensure(message, 'Group preview missing');
      const original = message.innerHTML;
      message.innerHTML = '<p class="responsive-long-text"></p><pre tabindex="0"><code></code></pre><div class="markdown-table-wrap" tabindex="0"><table><thead><tr></tr></thead><tbody><tr></tr></tbody></table></div>';
      message.querySelector('p').textContent = 'A long artifact path: ' + 'very_long_artifact_segment_'.repeat(18) + '.html';
      message.querySelector('code').textContent = 'const output = ' + 'wide_preformatted_column_'.repeat(16) + ';';
      for (let i = 0; i < 10; i++) {
        const th = document.createElement('th'), td = document.createElement('td');
        th.textContent = `Column ${i + 1}`; td.textContent = 'Review required';
        message.querySelector('thead tr').append(th); message.querySelector('tbody tr').append(td);
      }
      await pause(100);
      check('long prose wraps inside its message', () => {
        const p = message.querySelector('p');
        ensure(inside(message, stage) && p.scrollWidth <= p.clientWidth + 1, 'Long prose escaped the message width');
      });
      check('wide code and tables scroll within bounded surfaces', () => {
        for (const node of message.querySelectorAll('pre, .markdown-table-wrap')) {
          ensure(inside(node, stage), `${node.tagName} exceeds the pane`);
          ensure(node.scrollWidth > node.clientWidth, `${node.tagName} did not exercise wide content`);
          ensure(['auto', 'scroll'].includes(getComputedStyle(node).overflowX), `${node.tagName} has no scrolling`);
          node.scrollLeft = node.scrollWidth;
          ensure(node.scrollLeft > 0, `${node.tagName} cannot scroll to the final column`);
          node.scrollLeft = 0;
        }
        ensure(feed.scrollWidth <= feed.clientWidth + 1, 'Wide content escaped into the feed');
      });
      message.innerHTML = original;
    }
    await pause(100);
    feed.scrollTop = 0;
    return { ok: checks.every(c => c.ok), width: innerWidth, height: innerHeight, geometry: geometry(), checks };
  };
})();
