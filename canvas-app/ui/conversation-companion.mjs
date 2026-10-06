/** Selection/activity seam for the reviewed Blender atlas player.
 * Inject mountCompanion and reviewed {id, manifest} catalog entries at wiring
 * time. No default character, preference writes, event subscription or assets.
 * The caller forwards accepted live events using canonical owner/thread IDs;
 * transcript prose and replay are never animation evidence.
 */
export function createConversationCompanion({host, mountCompanion, catalog = [], onFallback = () => {}, onError = () => {}, size = 80, minimal = false} = {}) {
  if (!host || typeof mountCompanion !== 'function') throw new TypeError('A companion host and player factory are required');
  const characters = new Map(catalog.map(entry => [entry.id, entry.manifest]));
  let selected = null, token = null, generation = 0, player = null, choice = null;
  let disposed = false, readiness = 0, paused = false, phase = 'idle', terminal = null;
  const tools = new Map(), inputs = new Set();
  const alive = () => { if (disposed) throw new Error('Conversation companion has been destroyed'); };
  const currentState = () => {
    if (inputs.size || phase === 'queued' || phase === 'awaiting-input') return 'waiting';
    if (terminal) return terminal;
    return [...tools.values()].reverse().find(value => value === 'browsing' || value === 'coding') || 'idle';
  };
  function fallback(value) { onFallback(Boolean(value)); }
  function removePlayer() {
    readiness++;
    const old = player; player = null;
    old?.destroy();
    fallback(true);
  }
  function observeReady() {
    const instance = player, request = ++readiness, selection = token;
    if (!instance) return;
    instance.element.hidden = true; fallback(true);
    Promise.resolve(instance.ready).then(() => {
      if (disposed || player !== instance || request !== readiness || token !== selection) return;
      instance.element.hidden = false; instance.refresh(); fallback(false);
    }).catch(error => {
      if (disposed || player !== instance || request !== readiness || token !== selection) return;
      removePlayer(); onError(error);
    });
  }
  function sync(reobserve = false) {
    if (!player) return;
    const before = player.ready;
    player.setState(currentState());
    if (reobserve || player.ready !== before) observeReady();
  }
  function choose(characterId) {
    const next = characterId ?? null;
    if (choice === next && (player || !characters.has(next))) return;
    // Selecting a character is not a new terminal event. Keep current live
    // work/input evidence, but do not replay a past success/error accent.
    terminal = null;
    choice = next;
    const manifest = characters.get(next);
    if (!manifest || !selected) { removePlayer(); return; }
    try {
      if (player) player.setCharacter(manifest).setState(currentState());
      else player = mountCompanion(host, {character:manifest, state:currentState(), size, motion:minimal ? 'reduce' : 'auto', decorative:true});
      player.setPaused(paused);
      observeReady();
    } catch (error) { removePlayer(); onError(error); }
  }
  return Object.freeze({
    select({ownerId, conversationKey, turnId = '', characterId = null}) {
      alive();
      if (!ownerId || !conversationKey) throw new TypeError('Canonical owner and conversation IDs are required');
      const changedOwner = !selected || selected.ownerId !== ownerId || selected.conversationKey !== conversationKey;
      const changedTurn = changedOwner || selected.turnId !== turnId;
      if (changedTurn) {
        generation++;
        tools.clear(); inputs.clear(); phase = 'idle'; terminal = null;
        selected = {ownerId, conversationKey, turnId};
        token = Object.freeze({...selected, generation});
        if (changedOwner) { removePlayer(); choice = null; }
        else sync(true);
      }
      choose(characterId);
      return token;
    },
    setCharacter(characterId) { alive(); choose(characterId); },
    accept(lease, event) {
      alive();
      if (!selected?.turnId || lease !== token || event?.live !== true || event.ownerId !== selected.ownerId || event.conversationKey !== selected.conversationKey || event.turnId !== selected.turnId) return false;
      switch (event.kind) {
        case 'tool-start':
          // A late receipt cannot revive an ended/stopped turn. An accepted
          // working status or a newly selected turn can resume live activity.
          if (!event.operationId || ['completed', 'failed', 'stopped', 'interrupted'].includes(phase)) return false;
          if (!tools.has(event.operationId)) tools.set(event.operationId, event.activity === 'browsing' || event.activity === 'coding' ? event.activity : 'other');
          terminal = null;
          break;
        case 'tool-end':
          if (!tools.delete(event.operationId)) return false;
          break;
        case 'ask-open':
          if (!event.askId) return false;
          inputs.add(event.askId); terminal = null;
          break;
        case 'ask-close':
          if (!inputs.delete(event.askId)) return false;
          break;
        case 'status':
          if (!['working', 'queued', 'awaiting-input', 'stopped', 'interrupted'].includes(event.status)) return false;
          phase = event.status; terminal = null;
          if (phase === 'stopped' || phase === 'interrupted') tools.clear();
          break;
        case 'turn-ended':
          if (!['completed', 'failed', 'stopped', 'interrupted', 'queued', 'awaiting-input'].includes(event.status)) return false;
          tools.clear(); phase = event.status;
          terminal = event.status === 'failed' ? 'error' : event.status === 'completed' && event.success === true && !inputs.size ? 'success' : null;
          break;
        default: return false;
      }
      sync();
      return true;
    },
    setSize(value) { alive(); if (!Number.isFinite(value) || value < 16 || value > 256) throw new RangeError('Companion size must be 16–256'); size = value; player?.setSize(value); if (player) observeReady(); },
    setMinimal(value) { alive(); minimal = Boolean(value); player?.setMotion(minimal ? 'reduce' : 'auto'); },
    setPaused(value) { alive(); paused = Boolean(value); player?.setPaused(paused); },
    snapshot() { return Object.freeze({...selected, characterId:choice, state:currentState(), toolCount:tools.size, pendingInputs:inputs.size, mounted:Boolean(player)}); },
    destroy() { if (disposed) return; disposed = true; generation++; selected = token = null; tools.clear(); inputs.clear(); removePlayer(); },
  });
}
