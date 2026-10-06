// Default to Phoenix's configured source. A Codex-app login is a separate,
// explicitly requested test source, never a substitute for the connected pool.
export function acceptanceAuthOptions(flags = []) {
  let authSource = 'phoenix', accountPool = false, authProfile = null;
  const seen = new Set();
  for (const flag of flags) {
    const key = flag.startsWith('--profile=') ? '--profile' : flag;
    if (seen.has(key)) throw Error('Duplicate acceptance authentication option');
    seen.add(key);
    if (flag === '--provider-pool') accountPool = true;
    else if (flag === '--codex-login') authSource = 'codex';
    else if (key === '--profile') {
      authProfile = flag.slice('--profile='.length);
      if (!/^[a-zA-Z0-9][a-zA-Z0-9:._-]{0,159}$/.test(authProfile))
        throw Error('Acceptance profile must be an exact saved profile ID');
    } else throw Error('Supported authentication options: --provider-pool, --profile=ID, --codex-login');
  }
  if (Number(accountPool) + Number(Boolean(authProfile)) + Number(authSource === 'codex') > 1)
    throw Error('Choose one acceptance authentication source');
  return {authSource, accountPool, authProfile};
}

// Public provenance only. Selection is not proof of working login or quota.
// Never serialize credential objects, JWT claims, refresh tokens or raw errors.
export function acceptanceAccountProvenance(auth, accounts, authSource) {
  if (!['phoenix', 'codex'].includes(authSource)) throw Error('Unknown acceptance authentication source');
  return accounts.map((account, index) => {
    const id = authSource === 'phoenix'
      ? Object.keys(auth?.profiles || {}).find(id => auth.profiles[id] === account) : null;
    if (authSource === 'phoenix' && !id) throw Error('Selected account has no exact source identity');
    const label = id && typeof auth.labels?.[id] === 'string' ? auth.labels[id] : null;
    return {testProfileId:index ? `probe-${index + 1}` : 'probe', sourceProfileId:id,
      displayName:label ? label.replace(/[\u0000-\u001f\u007f-\u009f]/g, '').slice(0, 160) : null,
      source:authSource, accessExpiresAt:account.expires, authenticated:false, quotaChecked:false};
  });
}

// Select exactly the user's configured connection. Never fail over implicitly.
export function acceptanceAccount(auth, selected, now = Date.now(), provider) {
  // Provider-pool routes intentionally have no pinned profile. Mirror their
  // saved priority, selecting a usable access credential without refreshing or
  // copying any refresh token into the disposable test home.
  if (!selected && provider === 'openai-codex') {
    const available = Object.keys(auth.profiles || {}).filter(id => auth.profiles[id].provider === provider).sort();
    const savedOrder = auth.state?.order?.[`provider:${provider}`] || [];
    const order = [...new Set([...savedOrder.filter(id => available.includes(id)), ...available])];
    selected = order.find(id => {
      const credential = auth.profiles[id];
      return (credential.access || credential.token) && credential.expires > now + 30 * 60 * 1000;
    });
    if (!selected) throw Error('Configured Codex pool has no fresh access token for isolated acceptance; no task submitted');
  }
  if (typeof selected !== 'string' || !selected)
    throw Error('Phoenix has no configured auth profile; no task submitted');
  const saved = auth.profiles?.[selected];
  if (!saved || saved.provider !== 'openai-codex')
    throw Error('Configured Phoenix account is not a connected Codex profile; no task submitted');
  if (!(saved.access || saved.token) || !(saved.expires > now + 30 * 60 * 1000))
    throw Error('Configured Codex access token needs refresh before isolated acceptance; no task submitted');
  return saved;
}

export function acceptanceAccounts(auth, selected, now = Date.now(), provider) {
  if (selected || provider !== 'openai-codex') return [acceptanceAccount(auth, selected, now, provider)];
  const available = Object.keys(auth.profiles || {}).filter(id => auth.profiles[id].provider === provider).sort();
  const order = [...new Set([...(auth.state?.order?.[`provider:${provider}`] || []).filter(id => available.includes(id)), ...available])];
  const accounts = order.map(id => auth.profiles[id]).filter(account =>
    (account.access || account.token) && account.expires > now + 30 * 60 * 1000);
  if (!accounts.length) throw Error('Configured Codex pool has no fresh access token for isolated acceptance; no task submitted');
  return accounts;
}

// Explicit isolated-test route for a login refreshed in the Codex application.
// Retain only its current access token; never copy or rotate refresh credentials.
export function codexLoginAccount(auth, now = Date.now()) {
  const access = auth?.tokens?.access_token;
  if (typeof access !== 'string' || !access) throw Error('Codex has no saved subscription access token; no task submitted');
  let claims;
  try { claims = JSON.parse(Buffer.from(access.split('.')[1], 'base64url').toString('utf8')); }
  catch { throw Error('Codex access-token metadata is invalid; no task submitted'); }
  const expires = claims.exp * 1000;
  if (!Number.isFinite(expires) || expires <= now + 30 * 60 * 1000)
    throw Error('Codex access token needs refresh before isolated acceptance; no task submitted');
  if (!claims['https://api.openai.com/auth']?.chatgpt_account_id)
    throw Error('Codex access token has no subscription account identity; no task submitted');
  return {provider:'openai-codex', token:access, expires};
}
