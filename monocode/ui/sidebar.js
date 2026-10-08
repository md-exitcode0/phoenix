"use strict";

const $ = (id) => document.getElementById(id);
const TAURI = window.__TAURI__?.core ?? null;
const SIDEBAR_PREVIEW = new URLSearchParams(location.search).get("shot") === "sidebar";
const state = {
  view: null,
  selected: { kind: "agent", id: "phoenix" },
  query: "",
  showArchived: false,
  workingOnly: false,
  unreadOnly: false,
  collapsedSections: new Set(),
  dragging: null,
  gateway: { token: "", port: 17373 },
  refreshTimer: null,
  completionNotifications: new Set(),
  recentCompletionItems: new Map(),
  phoenixLogoSource: "assets/fluffy-butter-surprised.png",
};

const EMBER_CHECK_D = "M3.85 11.05C3.55 10.15 4.5 9.3 5.45 9.6L8.1 12.55 12.55 6.4 16.45 2.45C16.7 2.18 17.1 2.4 17.02 2.75L15.65 6.95 18.7 5.85C18.98 5.74 19.22 6.12 18.98 6.35L15.7 9.4 9.05 15.7C8.1 16.55 6.7 16.15 6.25 15L3.95 11.75C3.82 11.45 3.8 11.22 3.85 11.05Z";
const ICONS = {
  chevron: '<svg viewBox="0 0 20 20"><path d="m8 5 5 5-5 5"/></svg>',
  dots: '<svg viewBox="0 0 20 20"><circle cx="4" cy="10" r="1.25"/><circle cx="10" cy="10" r="1.25"/><circle cx="16" cy="10" r="1.25"/></svg>',
  pin: '<svg viewBox="0 0 20 20"><path d="m7 3 6 1-1 4 3 3-4 1-3 5-.4-5L4 9l4-1-1-5Z"/></svg>',
  archive: '<svg viewBox="0 0 20 20"><path d="M3 5h14v3H3zM5 8v8h10V8M8 11h4"/></svg>',
  edit: '<svg viewBox="0 0 20 20"><path d="m4 14-.5 3 3-.5L15 8l-2.5-2.5L4 14ZM11.8 6.2l2.5 2.5"/></svg>',
  copy: '<svg viewBox="0 0 20 20"><rect x="6" y="3" width="10" height="11" rx="2"/><rect x="3" y="6" width="10" height="11" rx="2"/></svg>',
  trash: '<svg viewBox="0 0 20 20"><path d="M4 6h12M8 3h4l1 3H7l1-3ZM6 6l.7 11h6.6L14 6M8.5 9v5M11.5 9v5"/></svg>',
  moon: '<svg viewBox="0 0 20 20"><path d="M16 12.7A7 7 0 0 1 7.3 4 6.5 6.5 0 1 0 16 12.7Z"/></svg>',
  sun: '<svg viewBox="0 0 20 20"><circle cx="10" cy="10" r="3.2"/><path d="M10 2v2M10 16v2M2 10h2M16 10h2M4.3 4.3l1.4 1.4M14.3 14.3l1.4 1.4M15.7 4.3l-1.4 1.4M5.7 14.3l-1.4 1.4"/></svg>',
  check: `<svg class="ember-check" viewBox="0 0 20 20" aria-hidden="true"><path d="${EMBER_CHECK_D}"/></svg>`,
};

function itemKey(kind, id) { return `${kind}:${id}`; }
function wireItem(kind, id) { return { kind, id }; }
function itemFromActivity(activity) { return activity?.item ?? { kind: "agent", id: "phoenix" }; }
function sameItem(a, b) { return a?.kind === b?.kind && a?.id === b?.id; }
function sameStringSet(left, right) {
  if (left.length !== right.length) return false;
  const values = new Set(left);
  return values.size === left.length && right.every((value) => values.has(value));
}
function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"]/g, (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[char]);
}
function clamp(value, low, high) { return Math.max(low, Math.min(high, value)); }
function snapSidebarWidth(value) { return clamp(272 + Math.round((Number(value) - 272) / 8) * 8, 272, 432); }
function slugId(value, fallback = "group") {
  const slug = String(value || "").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 48);
  return slug || fallback;
}
function hash(value) {
  let h = 2166136261;
  for (const ch of String(value)) { h ^= ch.charCodeAt(0); h = Math.imul(h, 16777619); }
  return h >>> 0;
}

function mockView() {
  const profiles = [
    ["phoenix", "Phoenix", "Chief of Staff", "#e55732", "phoenix", true],
    ["scribe", "Nico", "Communications & Inbox", "#3479d0", "paper-plane", true],
    ["planner", "Maya", "Calendar & Coordination", "#8a62c7", "violet-owl", false],
    ["finance", "Vera", "Finance & Purchasing", "#3c9a73", "mint-bear", false],
    ["coder", "Robin", "Engineering", "#e0782f", "ember-fox", true],
    ["frontend", "Leon Lin", "Product Design & Frontend", "#d94f86", "rose-cat", false],
    ["researcher", "Theo", "Research & Intelligence", "#3b8eaa", "blue-owl", false],
    ["presentation", "Elena", "Knowledge & Documents", "#b26a42", "copper-book", false],
    ["critic", "Remy", "Systems, Reliability & Security", "#6870cb", "indigo-eye", false],
    ["sales", "Owen", "Relationships & CRM", "#d04f51", "coral-dog", false],
    ["marketing", "June", "Publishing & Content", "#bf7a2c", "sunny-bird", false],
    ["personal_logistics", "Cleo", "Operations", "#527c66", "forest-bot", false],
  ];
  const now = Date.now();
  const agents = profiles.map(([agent_id, display_name, role_title, color, icon_seed, pinned], i) => ({
    agent_id, internal_role: agent_id === "phoenix" ? "orchestrator" : agent_id,
    display_name, role_title, description: `${display_name} owns ${role_title.toLowerCase()} for the company.`,
    color, icon_seed, kind: "responsibility_owner", lifecycle: "active", pinned,
    sort_order: i, canonical_session_id: `company-${agent_id}`, browser_profile_id: agent_id,
    archived_at: null, delete_after: null, created_at: new Date(now - 90e6).toISOString(), updated_at: new Date(now - i * 1300e3).toISOString(), as_of_seq: i + 1,
  }));
  const groups = [
    { group_id: "launch-room", name: "Launch Room", description: "Product, engineering, and audience working together.", color: "#e15d39", icon_seed: "launch", lifecycle: "active", pinned: true, sort_order: 20, canonical_session_id: "group-launch-room", metadata_json: "{}", archived_at: null, delete_after: null, created_at: new Date(now - 80e6).toISOString(), updated_at: new Date(now - 22e5).toISOString(), as_of_seq: 40 },
    { group_id: "back-office", name: "Back Office", description: "Finance, communications, and operations.", color: "#6870cb", icon_seed: "office", lifecycle: "active", pinned: false, sort_order: 21, canonical_session_id: "group-back-office", metadata_json: "{}", archived_at: null, delete_after: null, created_at: new Date(now - 70e6).toISOString(), updated_at: new Date(now - 72e5).toISOString(), as_of_seq: 41 },
  ];
  const members = [
    ...["frontend", "coder", "marketing", "phoenix"].map((agent_id, sort_order) => ({ group_id: "launch-room", agent_id, member_role: "member", history_access: "full", history_start_message_index: 0, sort_order })),
    ...["finance", "scribe", "planner"].map((agent_id, sort_order) => ({ group_id: "back-office", agent_id, member_role: "member", history_access: "full", history_start_message_index: 0, sort_order })),
  ];
  const relationships = [
    ["coder", "frontend", "asks for interaction judgment before shipping UI", "trusted"],
    ["frontend", "coder", "hands approved interface behavior into implementation", "coworker"],
    ["phoenix", "researcher", "requests evidence when a company decision depends on fresh facts", "trusted"],
    ["scribe", "planner", "coordinates commitments discovered in communication", "coworker"],
  ].map(([from_agent_id,to_agent_id,relationship,trust_level],index)=>({from_agent_id,to_agent_id,relationship,trust_level,policy_json:'{"shares_minimum_needed":true,"requires_task_scope":true}',updated_at:new Date(now-index*860e3).toISOString(),as_of_seq:30+index}));
  const details = {
    phoenix: ["working", "Coordinating the company", "openai", "gpt-5.6", true, "2m", ["Build the complete company backend", "Now make the new sidebar feel calm and alive", "Keep everything local and secure"]],
    scribe: ["working", "Triage inbox", "google", "gemini-3.1-pro", true, "4m", ["Catch up on the messages I missed"]],
    planner: ["idle", null, "anthropic", "claude-opus-4.6", false, "28m", ["Update the launch plan"]],
    finance: ["idle", null, "openai", "gpt-5.6", false, "1h", []],
    coder: ["working", "Indexing codebase", "anthropic", "claude-sonnet-4.6", true, "now", ["Harden the browser recovery path", "Reindex once the tests pass"]],
    frontend: ["idle", null, "google", "gemini-3.1-pro", false, "3h", ["Explore a cleaner settings system"]],
    researcher: ["idle", null, "xai", "grok-4.2", false, "5h", ["Research the best workflow recorders"]],
    presentation: ["idle", null, "openai", "gpt-5.6", false, "1d", []],
    critic: ["reviewing", "Reviewing browser evidence", "openai", "gpt-5.6", true, "8m", ["Check every browser recovery receipt"]],
    sales: ["idle", null, "anthropic", "claude-sonnet-4.6", false, "2d", []],
    marketing: ["idle", null, "google", "gemini-3.1-pro", true, "44m", ["Turn the launch notes into a campaign"]],
    personal_logistics: ["idle", null, "local", "qwen3.5", false, "3d", []],
  };
  const activities = agents.map((agent, i) => {
    const d = details[agent.agent_id];
    return { item: wireItem("agent", agent.agent_id), canonical_session_id: agent.canonical_session_id, title: d[6].at(-1) ?? agent.description, status: d[0], activity_label: d[1], active_agent_ids: d[0] === "working" ? [agent.agent_id] : [], provider_id: d[2], model: d[3], transcript_revision: d[4] ? 7 : 4, last_read_revision: 4, unread: d[4], modified_at: new Date(now - (i + 1) * 630e3).toISOString(), recent_prompts: d[6].map((preview, message_index) => ({ message_index, preview })) };
  });
  activities.push(
    { item: wireItem("group", "launch-room"), canonical_session_id: "group-launch-room", title: "Decide what ships in the first public build", status: "working", activity_label: "Iris and Leo are thinking", active_agent_ids: ["frontend", "coder"], provider_id: null, model: null, transcript_revision: 12, last_read_revision: 10, unread: true, modified_at: new Date(now - 210e3).toISOString(), recent_prompts: [{ message_index: 0, preview: "What should make the first public build?" }, { message_index: 7, preview: "Push on this until the tradeoffs are clear" }] },
    { item: wireItem("group", "back-office"), canonical_session_id: "group-back-office", title: "Keep accounts and operations caught up", status: "idle", activity_label: null, active_agent_ids: [], provider_id: null, model: null, transcript_revision: 4, last_read_revision: 4, unread: false, modified_at: new Date(now - 75e5).toISOString(), recent_prompts: [{ message_index: 0, preview: "Give me the weekly back-office brief" }] },
  );
  return { directory: { as_of_seq: 44, agents, groups, members, responsibilities: [], relationships, outside_call_grants: [], conversation_sources: [] }, activities, mutation: null };
}

function emptyView() {
  return { directory: { as_of_seq: 0, agents: [], groups: [], members: [], responsibilities: [], relationships: [], outside_call_grants: [], conversation_sources: [] }, activities: [], mutation: null };
}

function invoke(command, args = {}) { return TAURI.invoke(command, args); }
function wsUrl() { return `ws://127.0.0.1:${state.gateway.port}/?token=${encodeURIComponent(state.gateway.token)}`; }
async function directoryCommand(command) {
  if (!TAURI || SIDEBAR_PREVIEW) {
    if (command.action === "update_agent" && window.PhoenixFluffyFixture) await PhoenixFluffyFixture.save(command);
    return mockDirectoryCommand(command);
  }
  return new Promise((resolve, reject) => {
    let done = false;
    const socket = new WebSocket(wsUrl());
    const finish = (fn, value) => { if (done) return; done = true; clearTimeout(timer); try { socket.close(); } catch {} fn(value); };
    const timer = setTimeout(() => finish(reject, new Error("The company directory did not answer in time.")), 8000);
    socket.onopen = () => socket.send(JSON.stringify({ CompanyDirectory: command }));
    socket.onerror = () => finish(reject, new Error("Could not reach the Phoenix gateway."));
    socket.onmessage = (event) => {
      try {
        const message = JSON.parse(event.data);
        if (message.CompanyDirectory) finish(resolve, message.CompanyDirectory);
        else finish(reject, new Error(message.Error?.message ?? "Unexpected company-directory reply."));
      } catch (error) { finish(reject, error); }
    };
  });
}

function mockDirectoryCommand(command) {
  const view = structuredClone(state.view ?? mockView());
  const agents = view.directory.agents, groups = view.directory.groups;
  const locate = (item) => item.kind === "agent" ? agents.find((row) => row.agent_id === item.id) : groups.find((row) => row.group_id === item.id);
  if (command.action === "set_pinned") locate(command.item).pinned = command.pinned;
  if (command.action === "set_agent_lifecycle") agents.find((row) => row.agent_id === command.agent_id).lifecycle = command.lifecycle;
  if (command.action === "set_group_lifecycle") groups.find((row) => row.group_id === command.group_id).lifecycle = command.lifecycle;
  if (command.action === "mark_read") {
    const activity = view.activities.find((row) => sameItem(row.item, command.item));
    if (activity) { activity.last_read_revision = activity.transcript_revision; activity.unread = false; }
  }
  if (command.action === "reorder") command.items.forEach((item, index) => { const row = locate(item); if (row) row.sort_order = index; });
  if (command.action === "update_agent") {
    const row = agents.find((agent) => agent.agent_id === command.agent_id);
    Object.assign(row, { display_name: command.display_name, role_title: command.role_title, description: command.description, color: command.color, icon_seed: command.icon_seed });
    if (command.avatar) {
      let metadata = {}; try { metadata = JSON.parse(row.metadata_json || "{}"); } catch {}
      metadata.avatar = command.avatar; row.metadata_json = JSON.stringify(metadata);
    }
  }
  if (command.action === "update_group") Object.assign(groups.find((row) => row.group_id === command.group_id), { name: command.name, description: command.description, color: command.color, icon_seed: command.icon_seed }, command.leader_agent_id ? { leader_agent_id: command.leader_agent_id } : {});
  if (command.action === "set_group_leader") {
    if (!view.directory.members.some((row) => row.group_id === command.group_id && row.agent_id === command.leader_agent_id)) throw new Error("The leader must be a member of the group.");
    groups.find((row) => row.group_id === command.group_id).leader_agent_id = command.leader_agent_id;
  }
  if (command.action === "set_group_members") {
    view.directory.members = view.directory.members.filter((row) => row.group_id !== command.group_id);
    command.members.forEach((member, sort_order) => view.directory.members.push({
      group_id: command.group_id,
      agent_id: member.agent_id,
      member_role: member.member_role || "member",
      history_access: member.history_access || "full",
      history_start_message_index: 0,
      sort_order,
    }));
    const group = groups.find((row) => row.group_id === command.group_id);
    if (group && !command.members.some((member) => member.agent_id === group.leader_agent_id)) group.leader_agent_id = defaultGroupLeader(command.members.map((member) => member.agent_id));
  }
  if (command.action === "update_relationship") {
    const relationships=view.directory.relationships||(view.directory.relationships=[]);
    let row=relationships.find((entry)=>entry.from_agent_id===command.from_agent_id&&entry.to_agent_id===command.to_agent_id);
    if(!row){row={from_agent_id:command.from_agent_id,to_agent_id:command.to_agent_id,policy_json:'{"shares_minimum_needed":true,"requires_task_scope":true}',updated_at:new Date().toISOString(),as_of_seq:101};relationships.push(row);}
    Object.assign(row,{relationship:command.relationship,trust_level:command.trust_level,policy_json:command.policy_json||row.policy_json,updated_at:new Date().toISOString()});
  }
  if (command.action === "create_agent") {
    const id = (command.preferred_name || "new-coworker").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") + "-" + (agents.length + 1);
    agents.push({ agent_id: id, internal_role: id, display_name: command.preferred_name || "New coworker", role_title: "Phoenix is shaping this role", description: command.description, color: command.color || "#d46a43", icon_seed: id, kind: "responsibility_owner", lifecycle: "dormant", pinned: false, sort_order: agents.length + groups.length, canonical_session_id: `company-${id}`, browser_profile_id: id, metadata_json:JSON.stringify({avatar:command.avatar||{}}), archived_at: null, delete_after: null, created_at: new Date().toISOString(), updated_at: new Date().toISOString(), as_of_seq: 99 });
    view.activities.push({ item: wireItem("agent", id), canonical_session_id: `company-${id}`, title: "Phoenix is setting up this coworker", status: "setting_up", activity_label: "Learning the company", active_agent_ids: [], provider_id: "openai", model: null, transcript_revision: 0, last_read_revision: 0, unread: false, modified_at: new Date().toISOString(), recent_prompts: [] });
  }
  if(command.action==="clone_agent"){
    const source=agents.find((agent)=>agent.agent_id===command.source_agent_id);if(!source)throw new Error("Source coworker not found.");
    const name=String(command.preferred_name||`${source.display_name} Copy`).trim(),id=slugId(name,`coworker-${agents.length+1}`)+"-"+(agents.length+1),now=new Date().toISOString();
    let sourceMetadata={};try{sourceMetadata=JSON.parse(source.metadata_json||"{}");}catch{}
    agents.push({...structuredClone(source),agent_id:id,internal_role:id,display_name:name,lifecycle:"dormant",pinned:false,sort_order:agents.length+groups.length,canonical_session_id:`agent-${id}`,browser_profile_id:`agent-${id}`,metadata_json:JSON.stringify({provisioning:true,runtime_ready:false,cloned_from:source.agent_id,avatar:sourceMetadata.avatar||{}}),created_at:now,updated_at:now,as_of_seq:99});
    view.activities.push({item:wireItem("agent",id),canonical_session_id:`agent-${id}`,title:"Created · coworker record saved",status:"setting_up",activity_label:"Setting up · learning the role",active_agent_ids:[],provider_id:"openai",model:null,transcript_revision:0,last_read_revision:0,unread:false,modified_at:now,recent_prompts:[]});
  }
  if (command.action === "create_group") {
    const name = String(command.name || "New group").trim() || "New group", id = slugId(name, `group-${groups.length + 1}`);
    groups.push({ group_id: id, name, description: command.description, color: command.color, icon_seed: slugId(command.icon_seed, id), lifecycle: "active", pinned: false, sort_order: agents.length + groups.length, canonical_session_id: `group-${id}`, metadata_json: JSON.stringify(command.settings), leader_agent_id: command.members.includes(command.leader_agent_id) ? command.leader_agent_id : defaultGroupLeader(command.members), archived_at: null, delete_after: null, created_at: new Date().toISOString(), updated_at: new Date().toISOString(), as_of_seq: 100 });
    command.members.forEach((agent_id, sort_order) => view.directory.members.push({ group_id: id, agent_id, member_role: "member", history_access: "full", history_start_message_index: 0, sort_order }));
    view.activities.push({ item: wireItem("group", id), canonical_session_id: `group-${id}`, title: command.description, status: "idle", activity_label: null, active_agent_ids: [], provider_id: null, model: null, transcript_revision: 0, last_read_revision: 0, unread: false, modified_at: new Date().toISOString(), recent_prompts: [] });
  }
  return Promise.resolve(view);
}

function profileFor(item) {
  if (!state.view) return null;
  return item.kind === "agent"
    ? state.view.directory.agents.find((row) => row.agent_id === item.id)
    : state.view.directory.groups.find((row) => row.group_id === item.id);
}
function activityFor(item) { return state.view?.activities.find((row) => sameItem(row.item, item)); }
function displayName(item, profile = profileFor(item)) { return item.kind === "agent" ? profile?.display_name : profile?.name; }
function roleTitle(item, profile = profileFor(item)) { return item.kind === "agent" ? profile?.role_title : profile?.description; }
function profileColor(profile) { return profile?.color || "#d66542"; }
function isArchived(profile) { return ["archived", "pending_deletion"].includes(profile?.lifecycle); }

function phoenixLogoSource() { return state.phoenixLogoSource; }
function phoenixLogoMarkup(className="phoenix-raster-avatar") {
  return `<img class="${escapeHtml(className)}" data-phoenix-logo src="${escapeHtml(phoenixLogoSource())}" alt="">`;
}
function loadPhoenixLogo() {
  document.querySelectorAll("[data-phoenix-logo]").forEach((image)=>{image.src=state.phoenixLogoSource;});
}

const AGENT_PALETTE = Object.freeze([
  "#e55732","#f07a2a","#e8a01c","#d4b31a","#6fbf3a",
  "#2fa36b","#2aa89a","#2b93c7","#3b6fe0","#5b4fe0",
  "#8a4fd4","#c44bb8","#d94f86","#e04f5a","#b26a42",
  "#8a6a4a","#6b7280","#4b5563","#1f2937","#f3efe6",
]);
function parseHexColor(value) {
  const hex = String(value || "").trim().replace("#","");
  if (!/^[0-9a-fA-F]{6}$/.test(hex)) return [229, 87, 50];
  return [parseInt(hex.slice(0,2),16), parseInt(hex.slice(2,4),16), parseInt(hex.slice(4,6),16)];
}
function nearestPaletteColor(value) {
  const [r,g,b] = parseHexColor(value);
  let best = AGENT_PALETTE[0], bestDist = Infinity;
  for (const color of AGENT_PALETTE) {
    const [cr,cg,cb] = parseHexColor(color);
    const dist = (r-cr)**2 + (g-cg)**2 + (b-cb)**2;
    if (dist < bestDist) { bestDist = dist; best = color; }
  }
  return best;
}
function mixHex(hex, other, amount) {
  const [r,g,b] = parseHexColor(hex), [or,og,ob] = parseHexColor(other);
  const t = Math.max(0, Math.min(1, amount));
  const ch = (n) => Math.round(n).toString(16).padStart(2,"0");
  return `#${ch(r+(or-r)*t)}${ch(g+(og-g)*t)}${ch(b+(ob-b)*t)}`;
}
function fireDefs(gid, color) {
  // Fire burns hottest low and at the centre and cools toward the tips, so the
  // body ramp runs mostly VERTICALLY, with a brighter shoulder and a deeper base
  // than before. That contrast is what reads as "burning" at 26px.
  const gold = mixHex(color, "#ffd56a", .68);
  const coral = mixHex(color, "#ff6324", .08);
  const wine = mixHex(color, "#7a1228", .56);
  return `<defs><linearGradient id="${gid}a" x1="14" y1="4" x2="32" y2="45" gradientUnits="userSpaceOnUse"><stop stop-color="${gold}"/><stop offset=".42" stop-color="${coral}"/><stop offset="1" stop-color="${wine}"/></linearGradient><linearGradient id="${gid}b" x1="24" y1="16" x2="24" y2="44" gradientUnits="userSpaceOnUse"><stop stop-color="#fff6dc"/><stop offset=".55" stop-color="${mixHex(color,"#ffb347",.35)}"/><stop offset="1" stop-color="${coral}"/></linearGradient><linearGradient id="${gid}c" x1="24" y1="24" x2="24" y2="42" gradientUnits="userSpaceOnUse"><stop stop-color="#fffef6"/><stop offset="1" stop-color="${gold}"/></linearGradient></defs>`;
}

function fireLayers(gid, color, style) {
  const defs = fireDefs(gid, color);
  if (style === "bird_family") {
    return `${defs}<g class="phoenix-wings"><path fill="url(#${gid}a)" d="M24 9c-3.8-4.3-8.5-5.5-14.5-5.1 3.2 2.7 4.5 5 5 7.8-4.6-1.5-8.6-1-12.1 1.2 5.2 1.2 8.6 3.8 11.1 7.1-4.5-.2-7.9 1.2-10.4 4.2 5.4-.4 9.4 1.2 12.5 4.3l3.4-9.2L24 9Z"/><path fill="url(#${gid}a)" d="M26.2 9.1c4-4.1 8.8-5 14.7-4.2-3.3 2.5-4.8 4.8-5.4 7.5 4.7-1.2 8.6-.5 12 1.9-5.3.8-8.9 3.2-11.6 6.4 4.5.1 7.8 1.7 10.1 4.9-5.3-.8-9.5.4-12.8 3.3l-2.8-9.4-4.2-10.4Z"/><path fill="url(#${gid}a)" d="M18.3 17.1c1.6-5.6 5-9.7 10.4-12.2-.6 3.4-.2 6 1.4 8.2 2.6 3.6 4.1 7.4 2.7 12.2-.9 3.2-3.2 5.7-6.7 7.5 4.8 1.6 7.4 4.9 7.8 10-2.8-2.4-5.6-3.5-8.2-3.1-2.5.4-5.2 1.8-8.2 4.2.6-4.6 2.5-8.1 5.8-10.3-4.9-3.1-6.6-8.6-5-16.5Z"/><path fill="#fff6dc" d="M27.6 13.2c-2.1.2-3.8 1.3-5 3.1 2.7-.6 4.9 0 6.7 1.7-.1-1.8-.7-3.4-1.7-4.8Z"/><circle cx="27.5" cy="15.1" r="1.15" fill="#5b1427"/></g>`;
  }
  if (style === "character_glyphs") {
    return `${defs}<path fill="url(#${gid}a)" d="M23.4 51.6C14.2 49.6 7.6 42.8 8.2 33.6C8.6 27.2 12.4 21.6 16.8 14.2C13.6 16.8 13.2 10.4 17.6 5.2C18.4 11.4 20.4 16.6 23.4 12.2C23.2 24.8 23.4 38 23.4 51.6Z"/><path fill="url(#${gid}a)" d="M24.6 51.6C33.8 49.6 40.4 42.8 39.8 33.6C39.4 27.2 35.6 21.6 31.2 14.2C34.4 16.8 34.8 10.4 30.4 5.2C29.6 11.4 27.6 16.6 24.6 12.2C24.8 24.8 24.6 38 24.6 51.6Z"/><path fill="url(#${gid}b)" d="M23.6 46.4C17.8 45 14.2 40.6 14.6 35C15 30.6 17.6 26.8 20.4 21.8C18.8 23.6 18.8 19.4 21.4 16C21.8 20 22.8 23.2 23.6 20.4C23.5 29.2 23.6 38.2 23.6 46.4Z"/><path fill="url(#${gid}b)" d="M24.4 46.4C30.2 45 33.8 40.6 33.4 35C33 30.6 30.4 26.8 27.6 21.8C29.2 23.6 29.2 19.4 26.6 16C26.2 20 25.2 23.2 24.4 20.4C24.5 29.2 24.4 38.2 24.4 46.4Z"/><ellipse cx="24" cy="49.2" rx="8.4" ry="3.4" fill="url(#${gid}c)"/>`;
  }
  return `${defs}<path fill="url(#${gid}a)" d="M24 1.4C21.6 8.2 19.8 12.4 18.4 17.6C15.2 12.2 8.6 13.6 9.2 21.4C4.4 22.6 2.6 30.2 7.4 36.8C3.6 39.4 5.8 47.2 13.6 50.2C17.4 51.8 21 52.8 24 53C27 52.8 30.6 51.8 34.4 50.2C42.2 47.2 44.4 39.4 40.6 36.8C45.4 30.2 43.6 22.6 38.8 21.4C39.4 13.6 32.8 12.2 29.6 17.6C28.2 12.4 26.4 8.2 24 1.4Z"/><path fill="url(#${gid}b)" d="M24 13.2C22.4 17.6 21.2 20.4 20.4 24C18.4 20.6 14.4 21.6 14.8 26.6C11.8 27.4 10.8 32.2 13.8 36.4C11.6 38 12.8 43 17.8 44.8C20.2 45.8 22.2 46.4 24 46.5C25.8 46.4 27.8 45.8 30.2 44.8C35.2 43 36.4 38 34.2 36.4C37.2 32.2 36.2 27.4 33.2 26.6C33.6 21.6 29.6 20.6 27.6 24C26.8 20.4 25.6 17.6 24 13.2Z"/><path fill="url(#${gid}c)" d="M24 24.2C23.1 27.2 22.4 29.2 22 31.8C21.5 33.8 21.8 36.2 23 38C23.4 38.8 23.7 39.3 24 39.5C24.3 39.3 24.6 38.8 25 38C26.2 36.2 26.5 33.8 26 31.8C25.6 29.2 24.9 27.2 24 24.2Z"/>`;
}

// Every expression here is friendly. The old set led with angled inner brows
// ("focused", "determined") which read as scowling at avatar size, so those are
// gone and LEGACY_EXPRESSIONS remaps anything already saved.
const AVATAR_EXPRESSIONS = Object.freeze(["bright","joy","calm","curious","mischief","sleepy"]);
const LEGACY_EXPRESSIONS = Object.freeze({ focused:"curious", determined:"bright" });
const SIDEKICK_EXPRESSIONS = Object.freeze(["bright","joy","calm","curious","mischief","sleepy","focused","determined"]);
const SIDEKICK_FAMILIES = Object.freeze(["classic_flame","ember_orb","shard_flame","split_flame","halo_core","smoke_wisp"]);
const SIDEKICK_FAMILY_LABELS = Object.freeze({
  classic_flame:"Flame",
  ember_orb:"Ember",
  shard_flame:"Shard",
  split_flame:"Split",
  halo_core:"Halo",
  smoke_wisp:"Wisp",
});
const SIDEKICK_EYE_STYLES = Object.freeze(["round","spark","slit","visor","closed"]);
const LEGACY_SIDEKICK_MODELS = Object.freeze(["ember","cinder","kiln","wisp"]);
const LEGACY_SIDEKICK_FAMILY = Object.freeze({
  ember:"ember_orb",
  cinder:"smoke_wisp",
  kiln:"shard_flame",
  wisp:"halo_core",
});
// Three INDEPENDENT slots — a coworker can wear a top hat AND sunglasses AND
// headphones. The old single `accessory` field maps into whichever slot owned it.
const AVATAR_HATS = Object.freeze(["none","top_hat","beanie","cap","crown","halo"]);
const AVATAR_EYEWEAR = Object.freeze(["none","round_glasses","square_glasses","sunglasses","monocle"]);
const AVATAR_EXTRAS = Object.freeze(["none","headphones","spark","bowtie","scarf"]);
// Public capability map used by the avatar studio and its acceptance contract.
// Keeping the three slots explicit matters: a coworker can wear glasses, a hat,
// and headphones together instead of choosing one generic "accessory".
const AVATAR_ACCESSORIES = Object.freeze({
  hats: AVATAR_HATS,
  eyewear: AVATAR_EYEWEAR,
  extras: AVATAR_EXTRAS,
});
const LEGACY_ACCESSORY_SLOT = Object.freeze({
  round_glasses:"eyewear", square_glasses:"eyewear", sunglasses:"eyewear",
  headphones:"extra", spark:"extra",
});
const AVATAR_SHAPES = Object.freeze(["classic","soft","wild","tall"]);
const CURATED_AVATARS = Object.freeze({
  phoenix:{shape:"classic",expression:"bright",hat:"none",eyewear:"none",extra:"none"},
  orchestrator:{shape:"classic",expression:"bright",hat:"none",eyewear:"none",extra:"none"},
  scribe:{shape:"soft",expression:"curious",hat:"none",eyewear:"round_glasses",extra:"none"},
  planner:{shape:"tall",expression:"calm",hat:"none",eyewear:"none",extra:"spark"},
  finance:{shape:"soft",expression:"curious",hat:"top_hat",eyewear:"square_glasses",extra:"none"},
  coder:{shape:"wild",expression:"joy",hat:"beanie",eyewear:"square_glasses",extra:"headphones"},
  frontend:{shape:"soft",expression:"mischief",hat:"none",eyewear:"none",extra:"spark"},
  researcher:{shape:"tall",expression:"curious",hat:"none",eyewear:"round_glasses",extra:"none"},
  presentation:{shape:"classic",expression:"calm",hat:"none",eyewear:"none",extra:"bowtie"},
  critic:{shape:"wild",expression:"curious",hat:"none",eyewear:"monocle",extra:"none"},
  sales:{shape:"classic",expression:"bright",hat:"none",eyewear:"sunglasses",extra:"none"},
  marketing:{shape:"wild",expression:"mischief",hat:"cap",eyewear:"none",extra:"spark"},
  personal_logistics:{shape:"soft",expression:"calm",hat:"none",eyewear:"none",extra:"headphones"},
});
const customAvatarCache = new Map();

function backendAvatarMetadata(profile) {
  try {
    const metadata = JSON.parse(profile?.metadata_json || "{}");
    return metadata?.avatar && typeof metadata.avatar === "object" && !Array.isArray(metadata.avatar) ? metadata.avatar : {};
  } catch { return {}; }
}

function avatarMetadata(profile) { return window.PhoenixAvatarPreferences.effective(profile, backendAvatarMetadata(profile)); }

function inferredAvatar(profile, requestedStyle = null) {
  const id = String(profile?.agent_id || profile?.icon_seed || "phoenix");
  const seed = hash(`${id}:${profile?.icon_seed || id}`);
  const saved = avatarMetadata(profile);
  const role = String(profile?.internal_role || id).toLowerCase();
  const curated = CURATED_AVATARS[role] || CURATED_AVATARS[String(id).toLowerCase()] || null;
  const family = requestedStyle || document.documentElement.dataset.agentIcons || "flame_crests";
  const familyShape = { flame_crests:"classic", bird_family:"wild", character_glyphs:"soft" }[family] || "classic";
  // A pre-slots profile stored one `accessory`; route it to the slot it belonged
  // to so nobody loses their glasses when this ships.
  const legacySlot = LEGACY_ACCESSORY_SLOT[saved.accessory] || null;
  const slot = (name, allowed, fallback) => {
    if (allowed.includes(saved[name])) return saved[name];
    if (legacySlot === name) return saved.accessory;
    return fallback;
  };
  // Only two kinds remain: a Fluffy or a custom photo. Anything else saved
  // earlier (drawn marks, flames) shows as a Fluffy.
  const savedMode = saved.mode === "custom" && typeof saved.custom_image_id === "string" && saved.custom_image_id ? "custom" : "fluffy";
  const generatedSidekick = !saved.mode && id !== "phoenix";
  const savedExpression = savedMode === "sidekick"
    ? saved.expression
    : LEGACY_EXPRESSIONS[saved.expression] || saved.expression;
  const savedFamily = SIDEKICK_FAMILIES.includes(saved.family_id)
    ? saved.family_id
    : LEGACY_SIDEKICK_FAMILY[saved.model_id] || null;
  return {
    mode: savedMode,
    fluffy_palette: window.PhoenixAvatarPreferences.validColor(saved.fluffy_palette) ? saved.fluffy_palette : "butter",
    fluffy_shape: window.PhoenixAvatarPreferences.shapes.includes(saved.fluffy_shape) ? saved.fluffy_shape : "round",
    model_id: typeof saved.model_id === "string" ? saved.model_id : null,
    family_id: savedFamily || (savedMode === "sidekick" ? SIDEKICK_FAMILIES[seed % SIDEKICK_FAMILIES.length] : null),
    fiery: saved.fiery !== false,
    eye_style: SIDEKICK_EYE_STYLES.includes(saved.eye_style) ? saved.eye_style : generatedSidekick ? "closed" : "round",
    shape: AVATAR_SHAPES.includes(saved.shape) ? saved.shape : requestedStyle ? familyShape : curated?.shape || familyShape,
    expression: (savedMode === "sidekick" ? SIDEKICK_EXPRESSIONS : AVATAR_EXPRESSIONS).includes(savedExpression)
      ? savedExpression
      : curated?.expression || AVATAR_EXPRESSIONS[seed % AVATAR_EXPRESSIONS.length],
    hat: slot("hat", AVATAR_HATS, curated?.hat || "none"),
    eyewear: slot("eyewear", AVATAR_EYEWEAR, curated?.eyewear || "none"),
    extra: slot("extra", AVATAR_EXTRAS, curated?.extra || "none"),
    custom_image_id: typeof saved.custom_image_id === "string" ? saved.custom_image_id : null,
    morph_id: window.PhoenixMorphAvatar?.KINDS.includes(saved.morph_id) ? saved.morph_id : "phoenix",
    gradient: typeof saved.gradient === "string" && (saved.gradient === "custom" ? validGradientStops(saved.gradient_stops) : window.PhoenixMorphAvatar?.GRADIENTS.some(([gid]) => gid === saved.gradient))
      ? saved.gradient
      : id === "phoenix" ? "01-amber" : window.PhoenixMorphAvatar?.nearestGradient(profile?.color) || "01-amber",
    gradient_stops: validGradientStops(saved.gradient_stops) ? saved.gradient_stops : null,
  };
}
const validGradientStops = (stops) => Array.isArray(stops) && stops.length === 3 && stops.every((stop) => /^#[0-9a-f]{6}$/i.test(String(stop)));
function morphAvatarMarkup(avatar, agentId, extra = {}) {
  return window.PhoenixMorphAvatar?.markup({ kind:avatar.morph_id, gradient:avatar.gradient, stops:avatar.gradient_stops, agent:agentId, ...extra }) || phoenixLogoMarkup("flame-avatar phoenix-raster-avatar");
}

// Faces are drawn warm on purpose: rounded eyes with a highlight, mouths that
// curve up, and NO angled inner brows — at 26px a brow angled toward the nose
// is the single thing that turns a character sour.
function flameFace(expression) {
  const ink="#47211f", shine="#fffdf4", blush="#f2846b";
  const eye = (x,y=31.4,rx=1.7,ry=2.1) => `<ellipse cx="${x}" cy="${y}" rx="${rx}" ry="${ry}" fill="${ink}"/><circle cx="${x-.5}" cy="${y-.75}" r=".5" fill="${shine}"/>`;
  const round = (n) => Number(n.toFixed(2));
  const happyEye = (x,y=31.4) => `<path d="M${round(x-2.1)} ${round(y+.6)}c.8-1.6 3.4-1.6 4.2 0" fill="none" stroke="${ink}" stroke-width="1.5" stroke-linecap="round"/>`;
  const mouth = (d,w=1.35) => `<path d="${d}" fill="none" stroke="${ink}" stroke-width="${w}" stroke-linecap="round" stroke-linejoin="round"/>`;
  const cheeks = `<ellipse cx="15.6" cy="35" rx="1.9" ry="1.2" fill="${blush}" opacity=".38"/><ellipse cx="32.4" cy="35" rx="1.9" ry="1.2" fill="${blush}" opacity=".38"/>`;
  if (expression === "joy") return `${happyEye(19.7)}${happyEye(28.3)}${cheeks}<path d="M20.3 35.8c1.1 2.6 6.3 2.6 7.4 0Z" fill="${ink}"/>`;
  if (expression === "calm") return `${happyEye(19.7)}${happyEye(28.3)}${cheeks}${mouth("M21.2 36.9c1.7 1.5 3.9 1.5 5.6 0")}`;
  if (expression === "curious") return `${eye(19.5,31.6,1.55,1.95)}${eye(28.5,31.2,1.9,2.3)}${cheeks}${mouth("M22.4 37c1.1.9 2.6.9 3.7 0")}`;
  if (expression === "mischief") return `${happyEye(19.7)}${eye(28.4,31.4)}${cheeks}${mouth("M20.6 36.2c2.2 2.2 5.6 1.7 7-.9")}`;
  if (expression === "sleepy") return `${mouth("M17.6 31.6c1.4.9 2.8.9 4.1 0M26.3 31.6c1.4.9 2.8.9 4.1 0",1.4)}${cheeks}${mouth("M22.2 37.2c1.1-.7 2.5-.7 3.6 0")}<path d="m31.9 25 2.2-1.5-1.3 2.4 2.4.2-2.7 1.2" fill="none" stroke="${ink}" stroke-width=".9" stroke-linecap="round" stroke-linejoin="round"/>`;
  return `${eye(19.6)}${eye(28.4)}${cheeks}${mouth("M20.4 36.2c2.1 2.5 5.4 2.5 7.2 0")}`;
}

// Hats perch on the crest, ABOVE the face band (eyes ~y31, mouth ~y37), so they
// never collide with eyewear. Drawn behind nothing — they paint last.
function flameHat(hat) {
  // Shifted up ~6 units from the first pass: the crest tip reaches y=2, so a hat
  // sitting at y7-22 got pierced by the flame. These cap the crest instead.
  if (hat === "top_hat") return `<g><path d="M16.4 13.4V3.6c0-1.4 1.1-2.5 2.5-2.5h10.2c1.4 0 2.5 1.1 2.5 2.5v9.8Z" fill="#2e2833"/><path d="M16.4 9h15.2v3.3H16.4Z" fill="#c0472f"/><rect x="11.4" y="12.8" width="25.2" height="2.9" rx="1.45" fill="#241f28"/></g>`;
  if (hat === "beanie") return `<g><path d="M15.2 14.2c0-4.9 3.9-8.8 8.8-8.8s8.8 3.9 8.8 8.8Z" fill="#3f6f8e"/><rect x="14.4" y="13.2" width="19.2" height="3.2" rx="1.6" fill="#5b90b0"/><circle cx="24" cy="4.6" r="1.95" fill="#d6e8f1"/></g>`;
  if (hat === "cap") return `<g><path d="M15.6 14c0-4.6 3.8-8.4 8.4-8.4s8.4 3.8 8.4 8.4Z" fill="#c2553a"/><path d="M32.4 14h5.9c.9 0 1.4.8 1 1.5-.7 1.4-2.4 2.2-4.5 2.2h-2.4Z" fill="#9d3f2b"/><circle cx="24" cy="5.4" r="1.15" fill="#e8836a"/></g>`;
  if (hat === "crown") return `<g fill="#e8b23c"><path d="m14.8 14-1.6-9.4 5.2 3.5L24 1.8l5.6 6.3 5.2-3.5-1.6 9.4Z"/><rect x="14.4" y="13.7" width="19.2" height="2.7" rx="1.35"/><circle cx="24" cy="6.2" r="1.15" fill="#c0472f"/></g>`;
  if (hat === "halo") return `<g fill="none" stroke="#ffd968" stroke-width="1.6"><ellipse cx="24" cy="3.4" rx="7.2" ry="2.3"/></g>`;
  return "";
}
function flameEyewear(eyewear) {
  if (eyewear === "round_glasses") return `<g fill="none" stroke="#4a2725" stroke-width="1.35" stroke-linecap="round"><circle cx="19.5" cy="31.3" r="3.45"/><circle cx="28.5" cy="31.3" r="3.45"/><path d="M23 30.8h2M16 30.7l-2.4-.8M32 30.7l2.4-.8"/></g>`;
  if (eyewear === "square_glasses") return `<g fill="rgba(255,255,255,.12)" stroke="#4a2725" stroke-width="1.35" stroke-linejoin="round"><path d="M15.9 28.2h7v6.1h-5.5c-1-.5-1.5-1.4-1.5-2.7ZM25.1 28.2h7v3.4c0 1.3-.5 2.2-1.5 2.7h-5.5Z"/><path d="M22.9 30.4h2.2M15.9 29.8l-2.2-.7M32.1 29.8l2.2-.7" fill="none"/></g>`;
  if (eyewear === "sunglasses") return `<g fill="#382424" stroke="#382424" stroke-width="1.15" stroke-linejoin="round"><path d="M15.7 28.5h7.4v4.2c-1.6 1.4-4.8 1.3-6.3-.3Z"/><path d="M24.9 28.5h7.4l-1.1 3.9c-1.5 1.6-4.7 1.7-6.3.3Z"/><path d="M23.1 29.5h1.8M15.7 29.4l-2.1-.6M32.3 29.4l2.1-.6" fill="none"/><path d="m17.1 29.2 3.8 2.3M26.1 29.2l3.8 2.3" stroke="#fff" opacity=".32"/></g>`;
  if (eyewear === "monocle") return `<g fill="none" stroke="#4a2725" stroke-width="1.3" stroke-linecap="round"><circle cx="28.5" cy="31.3" r="3.6" fill="rgba(255,255,255,.14)"/><path d="M28.5 34.9v2.6"/></g>`;
  return "";
}
function flameExtra(extra) {
  if (extra === "headphones") return `<g fill="none" stroke="#4a2725" stroke-width="1.75" stroke-linecap="round"><path d="M14.7 31v-2.2c0-6.3 3.8-9.6 9.3-9.6s9.3 3.3 9.3 9.6V31"/><rect x="12.5" y="29.4" width="4.4" height="7.4" rx="2" fill="#4a2725"/><rect x="31.1" y="29.4" width="4.4" height="7.4" rx="2" fill="#4a2725"/></g>`;
  if (extra === "spark") return `<g><path d="m36 12.2 1.25 3.25 3.25 1.25-3.25 1.25L36 21.2l-1.25-3.25-3.25-1.25 3.25-1.25Z" fill="#fff8bf" stroke="#a83f2b" stroke-width=".75"/><circle cx="39.8" cy="23" r="1.05" fill="#ffd968"/></g>`;
  if (extra === "bowtie") return `<g fill="#c0472f"><path d="m24 41.6-5.1-2.3v4.9Z"/><path d="m24 41.6 5.1-2.3v4.9Z"/><circle cx="24" cy="41.6" r="1.25" fill="#8f3220"/></g>`;
  if (extra === "scarf") return `<g fill="#3f6f8e"><path d="M17.6 40.4c4.2 1.9 8.6 1.9 12.8 0l.6 2.9c-4.6 2.1-9.4 2.1-14 0Z"/><path d="M29.4 43.1l2.6.6-.7 3.4-2.7-.7Z" fill="#5b90b0"/></g>`;
  return "";
}

// A flame's temperament is derived from its seed, not randomised per render, so
// a coworker burns the same way every time you look at them while no two agents
// in a list ever move in lockstep. Delays are NEGATIVE: each fire starts partway
// through its own cycle, so a freshly painted sidebar is already mid-burn
// instead of visibly winding up together.
function fireTemperament(seed) {
  const pick = (shift, span, base) => base + ((seed >>> shift) % 1000) / 1000 * span;
  return `--fire-dur:${pick(3, 1.7, 2.7).toFixed(2)}s;--fire-delay:-${pick(11, 4, 0).toFixed(2)}s;--fire-amp:${pick(19, .7, .78).toFixed(2)};--spark-drift:${(pick(7, 4.4, -2.2)).toFixed(2)}px`;
}

function flameCharacterLayers(gid, color, shape, expression, worn, ambient = true) {
  const kit = typeof worn === "string" ? { hat:"none", eyewear:LEGACY_ACCESSORY_SLOT[worn] === "eyewear" ? worn : "none", extra:LEGACY_ACCESSORY_SLOT[worn] === "extra" ? worn : "none" } : (worn || {});
  const defs = fireDefs(gid, color);
  const outer = {
    classic:"M24 2c-1.2 6.2-6.9 9.4-5.5 16.2-4.1-4.2-10.8-.6-9.2 6.9-4.7 3.4-3.7 12.7 3 17.3 6.2 4.3 15.5 4.3 21.7 0 7.3-5.1 7.5-14.3 2.2-18.1C35.4 17.1 31 12.6 30.8 6c-2.2 2.1-4.5 4.4-6.8 7.3C25 8.7 24.8 5.2 24 2Z",
    // Rewritten 2026-08-23: the previous data had 55 curve arguments, and `c`
    // consumes 6 per segment — the orphan trailing number left the tail
    // undefined and WebKit drew it as a zigzag spike above the crest. This is
    // 7 complete curves (42 args) closing exactly on the start point.
    soft:"M24 3.4c-1.8 5.8-6.4 9.2-5.2 15.4-4.4-3.6-10.4.4-8.6 7.2-4.4 4.4-2.6 13.2 4.2 17.2 6 3.4 14.4 3.4 20.4 0 6.8-4 8.6-12.8 4.2-17.2 1.8-6.8-4.2-10.8-8.6-7.2 1.2-6.2-3.4-9.6-6.4-15.4Z",
    wild:"M23.5 1.5c-.5 6.8-7.7 8.5-5.3 17-6.9-6.3-12.4-.8-8.5 7.5-7.7 3.4-4.4 15.2 4.9 19 7.1 2.9 16.6 1.8 21.4-4.6 5.4-7.1 2.7-15.4-2.4-17.3 3.6-7.5-.2-12.3-3.8-15.7-.2 4.4-2.2 7.2-5.1 10.4 2-7.2.8-12.1-1.2-16.3Z",
    tall:"M25.1 1.4c-4.2 6.3-7.1 12.4-5.6 18.3-4.2-4.4-10.5-1.1-9.6 6.7-4 4.7-1.8 13.4 4.7 17.3 6.1 3.7 14.7 3.5 20.1-.9 6.8-5.6 6.7-15.6.3-19.2 2.5-6.7-1.9-11.2-5-14.9-.2 4.5-2.2 7.9-5.2 11 2-7 .8-13.1.3-18.3Z",
  }[shape] || null;
  const ember=mixHex(color,"#681827",.5), glow=mixHex(color,"#ffd86c",.72), tip=mixHex(color,"#ffb347",.42);
  // Tongues rise INSIDE the crest — the band above the eyes and below the tip —
  // and burn out before they reach the silhouette edge, so the fire reads as
  // moving within itself rather than sprouting antennae. Everything here is in
  // viewBox user units (48 wide), NOT screen pixels. Group faces opt out: at
  // four-up they are ~11px across and this is just noise.
  const licks = ambient ? `
    <g class="flame-licks" fill="${tip}">
      <path class="flame-lick lick-a" d="M24 18.4c-1.1 2.2-1.7 3.8-1.7 5.1 0 1.5.7 2.5 1.7 3.1 1-.6 1.7-1.6 1.7-3.1 0-1.3-.6-2.9-1.7-5.1Z"/>
      <path class="flame-lick lick-b" d="M18.7 21.2c-.8 1.6-1.2 2.8-1.2 3.7 0 1.1.5 1.8 1.2 2.2.7-.4 1.2-1.1 1.2-2.2 0-.9-.4-2.1-1.2-3.7Z"/>
      <path class="flame-lick lick-c" d="M29.3 20.4c-.8 1.6-1.2 2.8-1.2 3.7 0 1.1.5 1.8 1.2 2.2.7-.4 1.2-1.1 1.2-2.2 0-.9-.4-2.1-1.2-3.7Z"/>
    </g>
    <g class="flame-sparks" fill="${glow}">
      <circle class="flame-spark spark-a" cx="16.8" cy="24.4" r=".85"/>
      <circle class="flame-spark spark-b" cx="31.4" cy="22.6" r=".7"/>
      <circle class="flame-spark spark-c" cx="24.2" cy="19.4" r=".55"/>
    </g>` : "";
  return `${defs}
    <g class="flame-body">
      <path d="${outer}" fill="${ember}" opacity=".18" transform="translate(0 1.1)"/>
      <path fill="url(#${gid}a)" d="${outer}"/>
      <path fill="${mixHex(color,"#ff7b26",.2)}" opacity=".8" d="M10.5 26.1c3.6 1.2 6 3.7 7.2 7.4-2.7-1.1-5.2-1-7.6.5 1.5 5.4 5.3 9 10.7 10.8-7.8-.3-13.3-4.2-14.7-10.3-.8-3.3.8-6.7 4.4-8.4ZM37.5 24.9c-3.2 1.5-5.5 4-6.6 7.6 2.6-1.2 4.9-1.2 7.1.1-1 4.7-3.9 8.2-8.9 10.6 7.2-1 11.9-5 12.8-10.8.5-3.2-1-6-4.4-7.5Z"/>
      <g class="flame-core">
        <path fill="url(#${gid}b)" d="M24 12.5c-1.4 5.2-6.7 8.9-8.1 16-1.7 8.5 3.1 14.5 8.1 16.5 5-2 9.8-8 8.1-16.5-1.4-7.1-6.7-10.8-8.1-16Z"/>
        <path fill="url(#${gid}c)" d="M24 20.3c-1.2 3.9-5.8 7-6.2 12.6-.4 5.7 3.4 9.6 6.2 10.8 2.8-1.2 6.6-5.1 6.2-10.8-.4-5.6-5-8.7-6.2-12.6Z"/>
      </g>
      <path d="M22.1 16.7c-.6 3-2.7 5.1-4.5 7.5" fill="none" stroke="${glow}" stroke-width="1.25" stroke-linecap="round" opacity=".72"/>
      <circle cx="8.5" cy="21.5" r="1.1" fill="${glow}" opacity=".8"/><circle cx="39.2" cy="18.8" r=".75" fill="${glow}" opacity=".72"/>
      ${licks}
    </g>
    <g class="flame-face">${flameFace(expression)}${flameEyewear(kit.eyewear)}${flameExtra(kit.extra)}${flameHat(kit.hat)}</g>`;
}

function requestCustomAvatar(imageId) {
  if (window.PhoenixFluffyFixture?.image(imageId)) { customAvatarCache.set(imageId,PhoenixFluffyFixture.image(imageId)); return; }
  if (!TAURI || !imageId || customAvatarCache.has(imageId)) return;
  customAvatarCache.set(imageId, "loading");
  TAURI.invoke("avatar_data_url", { imageId }).then((src) => {
    customAvatarCache.set(imageId, src);
    document.querySelectorAll(`[data-avatar-id="${CSS.escape(imageId)}"]`).forEach((node) => { node.src = src; node.closest(".custom-avatar-shell")?.classList.add("ready"); });
  }).catch(() => customAvatarCache.set(imageId, "failed"));
}

let sidekickInstanceCounter = 0;
function sidekickCanvas(familyId, color, expression, fiery = true, eyeStyle = "round") {
  if (!SIDEKICK_FAMILIES.includes(familyId)) return '<span class="sidekick-unselected">Choose a form</span>';
  const runtime = globalThis.PhoenixSidekicks2D;
  if (runtime?.render) {
    try {
      return runtime.render({
        familyId,
        color,
        expression,
        fiery,
        eyeStyle,
        uid:`sk${++sidekickInstanceCounter}`,
      });
    } catch (error) {
      console.warn("Phoenix 2D sidekick render failed", error);
    }
  }
  const eye = eyeStyle === "closed"
    ? '<path d="M23 34h3M38 34h3" stroke="#4a2a25" stroke-width="1" stroke-linecap="round" opacity=".45"/>'
    : '<circle cx="24.5" cy="34" r="1.2" fill="#4a2a25" opacity=".5"/><circle cx="39.5" cy="34" r="1.2" fill="#4a2a25" opacity=".5"/>';
  return `<svg class="phoenix-sidekick2d" viewBox="0 0 64 64" aria-hidden="true"><path d="M32 7C30 18 23 23 24 33c-5-2-8 2-7 8 1 9 7 16 15 19 8-3 14-10 15-19 1-6-2-10-7-8 1-10-6-15-8-26Z" fill="${escapeHtml(color)}" opacity="${fiery ? ".96" : ".74"}"/>${eye}</svg>`;
}

function avatarSvg(profile, kind = "agent", requestedStyle = null, interactive = false) {
  if (kind === "group") return groupAvatarSvg(profile);
  const id = profile?.agent_id ?? "phoenix";
  const avatar = inferredAvatar(profile, requestedStyle);
  if (avatar.mode === "fluffy") return window.PhoenixFluffies.markup({ ...profile, ...avatar });
  if (avatar.mode !== "custom" || !avatar.custom_image_id) return window.PhoenixFluffies.markup({ ...profile, ...avatar, mode:"fluffy" });
  requestCustomAvatar(avatar.custom_image_id);
  const cached = customAvatarCache.get(avatar.custom_image_id);
  const src = cached && !["loading","failed"].includes(cached) ? ` src="${escapeHtml(cached)}"` : "";
  // While the photo loads, or if it cannot, a still Fluffy stands in.
  const fallback = `<span class="flame-avatar custom-avatar-fallback" aria-hidden="true" style="background-image:url('${escapeHtml(window.PhoenixFluffyFamily.poster(avatar.fluffy_shape, avatar.fluffy_palette))}')"></span>`;
  return `<span class="custom-avatar-shell ${src ? "ready" : ""}">${fallback}<img${src} data-avatar-id="${escapeHtml(avatar.custom_image_id)}" alt=""></span>`;
}

function colorField(name, current) {
  const selected = nearestPaletteColor(current || AGENT_PALETTE[0]);
  return `<div class="color-field"><input type="hidden" name="${name}" value="${escapeHtml(selected)}"><div class="color-swatches">${AGENT_PALETTE.map((color) => `<button type="button" class="color-swatch ${color === selected ? "selected" : ""}" data-color="${color}" style="--swatch:${color}" aria-label="${color}"></button>`).join("")}</div></div>`;
}
function bindColorField(root, onChange) {
  root?.querySelectorAll(".color-field").forEach((field) => {
    field.addEventListener("click", (event) => {
      const button = event.target.closest(".color-swatch");
      if (!button) return;
      field.querySelector("input").value = button.dataset.color;
      field.querySelector("input").dispatchEvent(new Event("input", { bubbles:true }));
      field.querySelectorAll(".color-swatch").forEach((item) => item.classList.toggle("selected", item === button));
      onChange?.(button.dataset.color);
    });
  });
}

// Two kinds of avatar: Fluffies first (the default), then a custom photo.
// The drawn morph marks are no longer offered; their hidden fields stay so a
// saved photo config keeps the shape the backend already accepts.
function avatarEditor(profile = null) {
  const avatar = inferredAvatar(profile || {});
  const mode = avatar.mode === "custom" ? "custom" : "fluffy";
  const tab = (value, label, hint) => `<button type="button" role="tab" data-avatar-mode-value="${value}" aria-pressed="${mode === value}" aria-selected="${mode === value}" class="${mode === value ? "selected" : ""}"><strong>${label}</strong><small>${hint}</small></button>`;
  return `<section class="avatar-studio" data-avatar-mode="${mode}">
    <div class="avatar-studio-preview"><i data-avatar-preview>${avatarSvg(profile || { agent_id:"new-coworker", color:"#e55732", icon_seed:"new" }, "agent", null, true)}</i><span><strong>Make them recognizable</strong><small data-avatar-preview-note>${mode === "custom" ? "Your own picture, cropped to a circle." : "A Fluffy that follows their activity."}</small></span></div>
    <nav class="avatar-mode-tabs" role="tablist" aria-label="Avatar style">${tab("fluffy", "Fluffies", "Animated companions")}${tab("custom", "Custom photo", "Upload your own")}</nav>
    <input type="hidden" name="avatar_mode" value="${mode}"><input type="hidden" name="avatar_morph_id" value="${escapeHtml(avatar.morph_id)}"><input type="hidden" name="avatar_gradient" value="${escapeHtml(avatar.gradient)}"><input type="hidden" name="custom_image_id" value="${escapeHtml(avatar.custom_image_id || "")}">
    ${window.PhoenixFluffies.editorMarkup(avatar.fluffy_palette,avatar.fluffy_shape)}
    <div class="avatar-custom-controls"><input class="avatar-file-fallback" type="file" accept="image/png,image/jpeg,image/webp,image/avif"><button class="avatar-drop" type="button" data-avatar-pick><span class="avatar-drop-icon" aria-hidden="true"><svg viewBox="0 0 24 24"><path d="M4 16.5V18a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-1.5M12 4v11M7.5 8.5 12 4l4.5 4.5"/></svg></span><span><strong data-avatar-pick-label>${avatar.custom_image_id ? "Choose a different photo" : "Choose a photo"}</strong><small>PNG, JPEG, WebP, or AVIF · up to 12 MB · cropped to a circle</small></span></button></div>
  </section>`;
}

function avatarFormValues(form) {
  const values = new FormData(form), gradient = String(values.get("avatar_gradient") || "01-amber");
  const stops = [0, 1, 2].map((i) => String(values.get(`avatar_gradient_stop_${i}`) || "").toLowerCase());
  return {
    ...(form._avatarOriginal || {}),
    mode:String(values.get("avatar_mode") || "fluffy"),
    fluffy_palette:String(values.get("fluffy_palette") || "butter"),
    fluffy_shape:String(values.get("fluffy_shape") || "round"),
    morph_id:String(values.get("avatar_morph_id") || "phoenix"),
    gradient,
    ...(gradient === "custom" && validGradientStops(stops) ? { gradient_stops:stops } : {}),
    custom_image_id:String(values.get("custom_image_id") || "") || null,
  };
}

// An agent's accent colour follows its avatar gradient (the middle stop).
function avatarAccentColor(form, config = avatarFormValues(form)) {
  return config.mode === "morph" ? window.PhoenixMorphAvatar?.stopsOf(config.gradient, config.gradient_stops)[1] || null : null;
}
function avatarFormProfile(form, original = null) {
  const values = new FormData(form);
  const config = avatarFormValues(form);
  const gradientColor = avatarAccentColor(form, config);
  return { ...(original || {}), _avatarDraft:true, agent_id:original?.agent_id || "new-coworker", icon_seed:original?.icon_seed || "new", color:String(values.get("color") || gradientColor || original?.color || "#e55732"), metadata_json:JSON.stringify({ ...(() => { try { return JSON.parse(original?.metadata_json || "{}"); } catch { return {}; } })(), avatar:config }) };
}

function bindAvatarEditor(form, original = null) {
  const studio = form.querySelector(".avatar-studio"); if (!studio) return;
  form._avatarOriginal = { ...backendAvatarMetadata(original) };
  const refresh = () => {
    const profile = avatarFormProfile(form, original);
    studio.dataset.avatarMode = form.elements.avatar_mode.value === "custom" ? "custom" : "fluffy";
    const custom = studio.dataset.avatarMode === "custom";
    studio.querySelectorAll("[data-avatar-mode-value]").forEach((button) => {
      const selected = button.dataset.avatarModeValue === studio.dataset.avatarMode;
      button.classList.toggle("selected", selected); button.setAttribute("aria-pressed", String(selected)); button.setAttribute("aria-selected", String(selected));
    });
    studio.querySelector("[data-avatar-preview-note]").textContent = custom ? "Your own picture, cropped to a circle." : "A Fluffy that follows their activity.";
    const hasPhoto = Boolean(form.dataset.customAvatarDataUrl || form.elements.custom_image_id.value);
    const pickLabel = studio.querySelector("[data-avatar-pick-label]"); if (pickLabel) pickLabel.textContent = hasPhoto ? "Choose a different photo" : "Choose a photo";
    const preview = studio.querySelector("[data-avatar-preview]");
    if (custom && form.dataset.customAvatarDataUrl) preview.innerHTML = `<span class="custom-avatar-shell ready"><img src="${escapeHtml(form.dataset.customAvatarDataUrl)}" alt="Custom avatar preview"></span>`;
    else if (custom && form.elements.custom_image_id.value) preview.innerHTML = avatarSvg(profile, "agent", null, true);
    else if (custom) preview.innerHTML = `<span class="avatar-photo-empty" aria-label="No photo chosen yet"><svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="9" r="3.6"/><path d="M5.5 19.5c1.3-3.3 3.8-5 6.5-5s5.2 1.7 6.5 5"/></svg></span>`;
    window.PhoenixFluffies.refreshEditor(form);
  };
  const applyCustomPreview = (dataUrl, imageId = "") => {
    if (dataUrl) form.dataset.customAvatarDataUrl = String(dataUrl);
    if (imageId) form.elements.custom_image_id.value = imageId;
    form.elements.avatar_mode.value = "custom"; refresh();
  };
  studio.addEventListener("click", (event) => {
    const mode = event.target.closest("[data-avatar-mode-value]")?.dataset.avatarModeValue;
    if (["custom", "fluffy"].includes(mode)) { form.elements.avatar_mode.value = mode; refresh(); return; }
    const fluffyShape = event.target.closest("[data-fluffy-shape-choice]")?.dataset.fluffyShapeChoice;
    if (fluffyShape && window.PhoenixFluffyFamily.available().includes(fluffyShape)) { form.elements.fluffy_shape.value = fluffyShape; if(!window.PhoenixFluffyFamily.palettes(fluffyShape).includes(form.elements.fluffy_palette.value))form.elements.fluffy_palette.value="butter"; form.elements.avatar_mode.value="fluffy"; refresh(); return; }
    const color = event.target.closest("[data-fluffy-color-choice]")?.dataset.fluffyColorChoice;
    if (window.PhoenixFluffyFamily.palettes(form.elements.fluffy_shape.value).includes(color)) { form.elements.fluffy_palette.value = color; form.elements.avatar_mode.value = "fluffy"; refresh(); return; }
  });
  const fallbackInput = studio.querySelector('input[type="file"]');
  studio.querySelector("[data-avatar-pick]").onclick = async () => {
    if (!TAURI || SIDEBAR_PREVIEW) { fallbackInput.click(); return; }
    const button = studio.querySelector("[data-avatar-pick]"); button.disabled = true;
    try {
      const imageId = await TAURI.invoke("avatar_pick"); if (!imageId) return;
      const dataUrl = await TAURI.invoke("avatar_data_url", { imageId });
      delete form.dataset.customAvatarDataUrl; applyCustomPreview(dataUrl, imageId);
    } catch (error) { toast(String(error), true); }
    finally { button.disabled = false; }
  };
  fallbackInput.addEventListener("change", (event) => {
    const file = event.target.files?.[0]; if (!file) return;
    if (file.size > 12 * 1024 * 1024) { toast("Custom avatar must be 12 MB or smaller.", true); event.target.value = ""; return; }
    const reader = new FileReader(); reader.onload = () => applyCustomPreview(String(reader.result || "")); reader.readAsDataURL(file);
  });
  refresh();
}

async function avatarConfigFromForm(form) {
  const config = avatarFormValues(form), { mode } = config;
  if (mode === "fluffy") { if(!window.PhoenixAvatarPreferences.validColor(config.fluffy_palette))throw Error("Choose a Fluffy color."); if(!window.PhoenixFluffyFamily.available().includes(config.fluffy_shape))throw Error("This native shape is still rendering."); if(config.fluffy_shape!=="round")await window.PhoenixFluffyFamily.resolve(config.fluffy_shape,config.fluffy_palette); return undefined; }
  delete config.fluffy_palette;
  delete config.fluffy_shape;
  if (mode !== "custom") throw new Error("Choose a Fluffy or a photo.");
  if (!window.PhoenixMorphAvatar?.KINDS.includes(config.morph_id)) throw new Error("Choose an avatar.");
  if (config.gradient === "custom" && !config.gradient_stops) throw new Error("Pick three colors for the custom gradient.");
  let customImageId = config.custom_image_id;
  if (mode === "custom" && form.dataset.customAvatarDataUrl) {
    if (TAURI && !SIDEBAR_PREVIEW) customImageId = await TAURI.invoke("avatar_import", { dataUrl:form.dataset.customAvatarDataUrl });
    else { customImageId = window.PhoenixFluffyFixture ? await PhoenixFluffyFixture.importImage(form.dataset.customAvatarDataUrl) : `avatar-preview-${Date.now()}.png`; customAvatarCache.set(customImageId, form.dataset.customAvatarDataUrl); }
  }
  if (mode === "custom" && !customImageId) throw new Error("Choose an image for this custom avatar.");
  return { ...config, custom_image_id:customImageId };
}
function openMenuSelect(anchor, items, selected, onPick, options = {}) {
  const searchable=Boolean(options.search),filterGroups=Boolean(options.filterGroups),groups=[];
  for(const item of items){const id=String(item[2]||"");if(id&&!groups.some((group)=>group.id===id))groups.push({id,label:String(item[3]||providerLabel(id))});}
  // Group metadata can describe ordinary navigation choices. Apply a group
  // restriction only when the menu also exposes a way to change that group.
  let activeGroup=filterGroups?String(options.initialGroup||items.find((item)=>String(item[0])===String(selected))?.[2]||groups[0]?.id||""):"";
  const rows=items.map(([value,label,group,,description])=>`<button type="button" data-value="${escapeHtml(value)}" data-menu-group="${escapeHtml(group||"")}" data-menu-search="${escapeHtml(`${label} ${description||""}`.toLowerCase())}" class="choice-row ${value === selected ? "selected" : ""}">${options.providerIcons?providerIcon(String(value)):""}<span><strong>${escapeHtml(label)}</strong>${description?`<small>${escapeHtml(description)}</small>`:""}</span>${value === selected ? ICONS.check : ""}</button>`).join("");
  const search=searchable?`<label class="menu-select-search"><svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="8.5" cy="8.5" r="5"/><path d="m12.3 12.3 4 4"/></svg><input type="search" placeholder="${escapeHtml(options.searchPlaceholder||"Search options")}" aria-label="${escapeHtml(options.searchLabel||options.searchPlaceholder||"Search options")}" autocomplete="off" spellcheck="false"></label>`:"";
  const groupFilters=filterGroups&&groups.length?`<div class="menu-provider-filters" role="tablist" aria-label="Provider">${groups.map((group)=>`<button type="button" role="tab" data-menu-provider="${escapeHtml(group.id)}" aria-selected="${group.id===activeGroup}">${providerIcon(group.id)}<span>${escapeHtml(group.label)}</span></button>`).join("")}</div>`:"";
  const extraClass=String(options.className||"").replace(/[^a-zA-Z0-9_-]/g,"");
  const pop = openPopover(anchor, `${groupFilters}${search}<div class="menu-select-options">${rows}<div class="menu-select-empty" hidden>No matching options.</div></div>`, `menu-select-popover${searchable?" searchable":""}${groupFilters?" provider-filtered":""}${extraClass?` ${extraClass}`:""}`, { align: "start", matchWidth: true, minWidth:options.minWidth||0 });
  const input=pop.querySelector(".menu-select-search input"),buttons=[...pop.querySelectorAll("[data-menu-search]")],empty=pop.querySelector(".menu-select-empty"),providerButtons=[...pop.querySelectorAll("[data-menu-provider]")];
  const filter=()=>{const words=String(input?.value||"").trim().toLowerCase().split(/\s+/).filter(Boolean);let visible=0;buttons.forEach((button)=>{const match=(!activeGroup||button.dataset.menuGroup===activeGroup)&&words.every((word)=>button.dataset.menuSearch.includes(word));button.hidden=!match;if(match)visible+=1;});empty.hidden=visible!==0;};
  if(input){input.oninput=filter;requestAnimationFrame(()=>input.focus({preventScroll:true}));}filter();
  pop.onclick = (event) => {
    const providerButton=event.target.closest("[data-menu-provider]");
    if(providerButton){activeGroup=providerButton.dataset.menuProvider;providerButtons.forEach((button)=>button.setAttribute("aria-selected",String(button===providerButton)));filter();return;}
    const value = event.target.closest("button")?.dataset.value;
    if (value == null) return;
    onPick(value);
    closeLayers();
  };
}

function groupAvatarSvg(profile) {
  return `<svg class="group-symbol" viewBox="0 0 28 28" aria-hidden="true" style="color:${escapeHtml(profileColor(profile))}"><circle cx="14" cy="8" r="3.2"/><path d="M7.8 23v-2.5a6.2 6.2 0 0 1 12.4 0V23M6.5 6.5a3 3 0 0 0 0 6M4 22v-3a5 5 0 0 1 3.3-4.7M21.5 6.5a3 3 0 0 1 0 6M24 22v-3a5 5 0 0 0-3.3-4.7"/></svg>`;
}

const PROVIDER_BADGES = Object.freeze({
  openrouter:["OR","#6b5cff"],tokenrouter:["TR","#e45d35"],opencode:["OC","#292827"],deepseek:["DS","#3568f0"],groq:["G","#f55036"],mistral:["M","#f28a22"],"minimax-portal":["MM","#ed4b67"],together:["T","#2928d8"],fireworks:["FW","#d23b31"],deepinfra:["DI","#4f63df"],"kimi-coding":["K","#7158e8"],moonshot:["MO","#171716"],zai:["Z","#315cff"],"github-copilot":["GH","#24292f"],cerebras:["C","#f15c36"],venice:["V","#b61f32"],kilocode:["K","#4a7bf7"],meta:["∞","#1473e6"],nvidia:["NV","#76b900"],"ollama-cloud":["O","#262524"],volcengine:["V","#2f65f5"],byteplus:["BP","#6848e8"],stepfun:["S","#2f66df"],qianfan:["Q","#2d70e8"],tencent:["T","#176bd5"],xiaomi:["MI","#ff6900"],chutes:["C","#7a55da"],sglang:["SG","#346ee8"],vllm:["V","#6b49c8"],lm_studio:["LM","#242323"],litellm:["LL","#4b77e5"],huggingface:["HF","#f2b51d"],"hf-deepseek-v4-free":["DS","#3568f0"],server:["••","#77736d"]
});

function providerKey(providerId, modelId = "") {
  const provider=String(providerId||"server").toLowerCase().replaceAll(" ","_"),model=String(modelId||"").toLowerCase();
  if(/claude/.test(model))return"claude";if(/gemini/.test(model))return"gemini";if(/grok/.test(model))return"grok";
  if(provider==="anthropic")return"anthropic";if(["openai","openai-codex"].includes(provider))return"openai";if(["google","google-gemini-cli"].includes(provider))return"gemini";if(provider==="xai")return"xai";if(provider==="grok-cli")return"grok";if(["ollama","local"].includes(provider))return"local";
  return PROVIDER_BADGES[provider]?provider:"server";
}

function providerBadgeSvg(provider){const[label,color]=PROVIDER_BADGES[provider]||PROVIDER_BADGES.server;return`<svg class="provider-mark provider-badge" viewBox="0 0 24 24" aria-hidden="true"><rect x="1.5" y="1.5" width="21" height="21" rx="6" fill="${color}"/><text x="12" y="12.5" fill="white" text-anchor="middle" dominant-baseline="middle" font-family="ui-sans-serif,system-ui,sans-serif" font-size="${label.length>2?6.6:label.length===2?7.8:10.5}" font-weight="800" letter-spacing="-.35">${label}</text></svg>`;}

function providerIcon(providerId, modelId = "") {
  const p = providerKey(providerId,modelId);
  const svg = {
    openai:'<svg class="provider-mark brand-fill" viewBox="0 0 24 24"><path d="M9.205 8.658v-2.26c0-.19.072-.333.238-.428l4.543-2.616c.619-.357 1.356-.523 2.117-.523 2.854 0 4.662 2.212 4.662 4.566 0 .167 0 .357-.024.547l-4.71-2.759a.797.797 0 0 0-.856 0l-5.97 3.473Zm10.609 8.8V12.06c0-.333-.143-.57-.429-.737l-5.97-3.473 1.95-1.118a.433.433 0 0 1 .476 0l4.543 2.617c1.309.76 2.189 2.378 2.189 3.948 0 1.808-1.07 3.473-2.76 4.163ZM7.802 12.703l-1.95-1.142c-.167-.095-.239-.238-.239-.428V5.899c0-2.545 1.95-4.472 4.591-4.472 1 0 1.927.333 2.712.928L8.23 5.067c-.285.166-.428.404-.428.737v6.898ZM12 15.128l-2.795-1.57v-3.33L12 8.658l2.795 1.57v3.33L12 15.128Zm1.796 7.23c-1 0-1.927-.332-2.712-.927l4.686-2.712c.285-.166.428-.404.428-.737v-6.898l1.974 1.142c.167.095.238.238.238.428v5.233c0 2.545-1.974 4.472-4.614 4.472Zm-5.637-5.303-4.544-2.617c-1.308-.761-2.188-2.378-2.188-3.948A4.482 4.482 0 0 1 4.21 6.327v5.423c0 .333.143.571.428.738l5.947 3.449-1.95 1.118a.432.432 0 0 1-.476 0Zm-.262 3.9c-2.688 0-4.662-2.021-4.662-4.519 0-.19.024-.38.047-.57l4.686 2.71c.286.167.571.167.856 0l5.97-3.448v2.26c0 .19-.07.333-.237.428l-4.543 2.616c-.619.357-1.356.523-2.117.523Zm5.899 2.83a5.947 5.947 0 0 0 5.827-4.756C22.287 18.339 24 15.84 24 13.296c0-1.665-.713-3.282-1.998-4.448.119-.5.19-.999.19-1.498 0-3.401-2.759-5.947-5.946-5.947-.642 0-1.26.095-1.88.31A5.962 5.962 0 0 0 10.205 0a5.947 5.947 0 0 0-5.827 4.757C1.713 5.447 0 7.945 0 10.49c0 1.666.713 3.283 1.998 4.448-.119.5-.19 1-.19 1.499 0 3.401 2.759 5.946 5.946 5.946.642 0 1.26-.095 1.88-.309a5.96 5.96 0 0 0 4.162 1.713Z"/></svg>',
    anthropic:'<svg class="provider-mark brand-fill" viewBox="0 0 24 24"><path d="M13.827 3.52h3.603L24 20h-3.603l-6.57-16.48Zm-7.258 0h3.767L16.906 20h-3.674l-1.343-3.461H5.017L3.673 20H0L6.57 3.522Zm4.132 9.959L8.453 7.687 6.205 13.48H10.7Z"/></svg>',
    claude:'<svg class="provider-mark brand-fill" viewBox="0 0 24 24"><path d="m4.709 15.955 4.72-2.647.08-.23-.08-.128H9.2l-.79-.048-2.698-.073-2.339-.097-2.266-.122-.571-.121L0 11.784l.055-.352.48-.321.686.06 1.52.103 2.278.158 1.652.097 2.449.255h.389l.055-.157-.134-.098-2.461-1.693-3.888-2.66-.724-.491-.364-.462-.158-1.008.656-.722.881.06.225.061.893.686 4.399 3.309.365.304.145-.103.019-.073-.164-.274-2.801-4.936-.644-1.032-.274-1.348L6.283.134 6.696 0l.996.134.42.364.62 1.414 2.557 5.259.699 1.73.091.255h.158l.602-6.642.08-.76.376-.91.747-.492.584.28.48.685-.067.444-1.209 6.696h.212l.243-.242 3.367-4.19 1.397-1.335h1.033l.76 1.129-.34 1.166-3.999 5.553.073.11.188-.02 6.24-1.201.833.388.091.395-.328.807-7.717 1.761-.042.03.049.061 2.211.182h1.622l3.02.225.79.522.474.638-.079.485-1.215.62-6.781-1.628h-.182v.11l5.608 5.208.127.578-.322.455-.34-.049-4.982-4.024h-.128v.17l2.789 4.17.122 1.08-.17.353-.608.213-.668-.122-3.932-6.035-.14.08-.674 7.254-.316.37-.729.28-.607-.461-.322-.747 1.401-6.829-.012-.042-.14.018-5.34 7.721-1.726 1.845-.414.164-.717-.37.067-.662.401-.589 4.758-6.004-.006-.158h-.055L4.132 18.56l-1.13.146-.487-.456.061-.746.231-.243 1.908-1.312-.006.006Z"/></svg>',
    gemini:'<svg class="provider-mark gemini-mark" viewBox="0 0 24 24"><path d="M20.616 10.835a14.147 14.147 0 0 1-4.45-3.001 14.111 14.111 0 0 1-3.678-6.452.503.503 0 0 0-.975 0 14.134 14.134 0 0 1-3.679 6.452 14.155 14.155 0 0 1-4.45 3.001c-.65.28-1.318.505-2.002.678a.502.502 0 0 0 0 .975c.684.172 1.35.397 2.002.677a14.147 14.147 0 0 1 4.45 3.001 14.112 14.112 0 0 1 3.679 6.453.502.502 0 0 0 .975 0c.172-.685.397-1.351.677-2.003a14.145 14.145 0 0 1 3.001-4.45 14.113 14.113 0 0 1 6.453-3.678.503.503 0 0 0 0-.975 13.245 13.245 0 0 1-2.003-.678Z"/></svg>',
    xai:'<svg class="provider-mark brand-fill" viewBox="0 0 24 24"><path d="M6.469 8.776 16.512 23h-4.464L2.005 8.776H6.47Zm-.004 7.9L8.698 19.84 6.467 23H2l4.465-6.324ZM22 2.582V23h-3.659V7.764L22 2.582ZM22 1l-9.952 14.095-2.233-3.163L17.533 1H22Z"/></svg>',
    grok:'<svg class="provider-mark brand-fill" viewBox="0 0 24 24"><path d="m9.27 15.29 7.978-5.897c.391-.29.95-.177 1.137.272.98 2.369.542 5.215-1.41 7.169-1.951 1.954-4.667 2.382-7.149 1.406l-2.711 1.257c3.889 2.661 8.611 2.003 11.562-.953 2.341-2.344 3.066-5.539 2.388-8.42l.006.007c-.983-4.232.242-5.924 2.75-9.383.06-.082.12-.164.179-.248l-3.301 3.305v-.01L9.267 15.292m-1.644 1.431c-2.792-2.67-2.31-6.801.071-9.184 1.761-1.763 4.647-2.483 7.166-1.425l2.705-1.25a7.808 7.808 0 0 0-1.829-1A8.975 8.975 0 0 0 5.984 5.83c-2.533 2.536-3.33 6.436-1.962 9.764 1.022 2.487-.653 4.246-2.34 6.022-.599.63-1.199 1.259-1.682 1.925l7.62-6.815"/></svg>',
    local:'<svg viewBox="0 0 20 20"><rect x="5" y="5" width="10" height="10" rx="2"/><path d="M8 2v3M12 2v3M8 15v3M12 15v3M2 8h3M15 8h3M2 12h3M15 12h3"/></svg>',
    router:'<svg viewBox="0 0 20 20"><circle cx="4" cy="10" r="2"/><circle cx="15" cy="5" r="2"/><circle cx="15" cy="15" r="2"/><path d="m6 9 7-3M6 11l7 3"/></svg>',
    server:'<svg viewBox="0 0 20 20"><rect x="3" y="3" width="14" height="5" rx="2"/><rect x="3" y="12" width="14" height="5" rx="2"/><path d="M6 5.5h.1M6 14.5h.1"/></svg>',
  }[p] || providerBadgeSvg(p);
  const title = [providerId, modelId].filter(Boolean).join(" · ") || "Provider unavailable";
  return `<span class="provider-chip" data-provider="${p}" title="${escapeHtml(title)}">${svg}</span>`;
}

function providerLabel(providerId) {
  const id = String(providerId || "custom").trim().toLowerCase();
  const names = {
    openai: "OpenAI API",
    "openai-codex": "OpenAI Codex",
    anthropic: "Anthropic",
    xai: "xAI",
    "grok-cli": "Grok CLI",
    google: "Google",
    "google-gemini-cli": "Gemini CLI",
    "github-copilot": "GitHub Copilot",
    ollama: "Ollama",
    "ollama-cloud": "Ollama Cloud",
    openrouter: "OpenRouter",
    lm_studio: "LM Studio",
    huggingface: "Hugging Face",
  };
  if (names[id]) return names[id];
  return id.replaceAll("_", "-").split("-").filter(Boolean).map((part) => {
    const acronym = { api: "API", ai: "AI", hf: "HF", llm: "LLM", sglang: "SGLang", vllm: "vLLM" }[part];
    return acronym || part[0].toUpperCase() + part.slice(1);
  }).join(" ") || "Custom provider";
}

function formatTime(value) {
  if (!value) return "";
  const elapsed = Date.now() - new Date(value).getTime();
  if (!Number.isFinite(elapsed) || elapsed < 0) return "now";
  if (elapsed < 60e3) return "now";
  if (elapsed < 3600e3) return `${Math.floor(elapsed / 60e3)}m`;
  if (elapsed < 86400e3) return `${Math.floor(elapsed / 3600e3)}h`;
  if (elapsed < 604800e3) return `${Math.floor(elapsed / 86400e3)}d`;
  return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(new Date(value));
}

function allRows() {
  if (!state.view) return [];
  const rows = [
    ...state.view.directory.agents.filter((p) => p.kind === "responsibility_owner").map((profile) => ({ kind: "agent", id: profile.agent_id, profile })),
    ...state.view.directory.groups.map((profile) => ({ kind: "group", id: profile.group_id, profile })),
  ];
  return rows.map((row) => ({ ...row, activity: activityFor(row) })).sort((a,b) => Number(b.profile.pinned)-Number(a.profile.pinned) || a.profile.sort_order-b.profile.sort_order || displayName(a,a.profile).localeCompare(displayName(b,b.profile)));
}

// These are execution states, not every state with unfinished work. Queued
// assignments and waits must not look like an agent is currently running.
const WORKING_ACTIVITY_STATES = new Set(["working", "reviewing", "reasoning", "using_tool", "starting", "forming", "integrating"]);
const ACTIVITY_PRESENTATION = Object.freeze({
  working: { label: "Working" },
  reviewing: { label: "Reviewing" },
  reasoning: { label: "Thinking" },
  using_tool: { label: "Using a tool" },
  starting: { label: "Starting" },
  forming: { label: "Setting up" },
  integrating: { label: "Combining results" },
  setting_up: { label: "Setting up", tone: "waiting" },
  queued: { label: "Queued", tone: "waiting" },
  waiting_peer: { label: "Waiting for a coworker", tone: "waiting" },
  waiting_user: { label: "Waiting for you", tone: "attention", notice: "needs your input" },
  blocked: { label: "Needs attention", tone: "error", notice: "needs attention" },
  failed: { label: "Needs attention", tone: "error", notice: "needs attention" },
  incomplete: { label: "Needs attention", tone: "error", notice: "needs attention" },
  unavailable: { label: "Unavailable", tone: "error", notice: "needs attention" },
  stale: { label: "Needs attention", tone: "error", notice: "needs attention" },
  completed_unverified: { label: "Needs review", tone: "attention", notice: "needs review" },
  completed_verified: { label: "Completed", notice: "finished" },
  canceled: { label: "Stopped", notice: "stopped" },
  cancelled: { label: "Stopped", notice: "stopped" },
  stopped: { label: "Stopped", notice: "stopped" },
  archived: { label: "Archived" },
  pending_deletion: { label: "Pending deletion" },
  superseded: { label: "Superseded" },
});

function sidebarActivity(row) {
  const activity = row.activity, status = String(activity?.status || "idle");
  const presentation = ACTIVITY_PRESENTATION[status];
  const working = WORKING_ACTIVITY_STATES.has(status);
  const detail = activity?.activity_label ? humanActivityText(activity.activity_label) : "";
  const label = presentation?.label || "";
  const text = working ? detail || label : label
    ? label + (detail && detail.toLowerCase() !== label.toLowerCase() ? ` · ${detail}` : "")
    : humanActivityText(activity?.title || roleTitle(row, row.profile) || "Ready when you are");
  return { status, working, text, tone: working ? "working" : presentation?.tone || "idle" };
}

function rowMatches(row) {
  const query = state.query.trim().toLowerCase();
  if (!state.showArchived && isArchived(row.profile)) return false;
  if (state.workingOnly && !WORKING_ACTIVITY_STATES.has(row.activity?.status)) return false;
  if (state.unreadOnly && !row.activity?.unread) return false;
  if (!query) return true;
  return [displayName(row,row.profile), roleTitle(row,row.profile), row.activity?.title, sidebarActivity(row).text].some((value) => String(value || "").toLowerCase().includes(query));
}

function humanActivityText(value) {
  // Chat-app messages carry a "[via Telegram · Name]" marker for the coworker.
  let text=String(value||"").trim().replace(/^\[via (?:Telegram|Discord) · [^\]\n]{1,80}\]\s*/,"").replace(/^\[background return\]\s*/i,"").replace(/\bbackground specialist\b/gi,"coworker").replace(/\bspecialist\b/gi,"coworker");
  if(/^\[late ask answer\]/i.test(text))return"Question answered · ready to continue";
  if(/^\[queued wake\]/i.test(text)||/^queued prompt queued_/i.test(text))return"Queued work is ready";
  if(/^Initiating\s+[a-z0-9_-]+\s+call\.?$/i.test(text))return"Starting the next action";
  if(/^Phoenix stopped this agent at a hard runtime boundary:/i.test(text))return"Run stopped before finishing";
  for(const agent of state.view?.directory.agents||[]){const role=String(agent.internal_role||"").trim();if(role&&role!==agent.display_name&&role!==agent.agent_id)text=text.replace(new RegExp(`\\b${role.replace(/[.*+?^${}()|[\]\\]/g,"\\$&")}\\b`,"gi"),agent.display_name);}
  if(/^A coworker finished while/i.test(text))return"Background work finished";
  return text||"Ready when you are";
}

// Identity stays one text run. Moving individual glyphs crosses the row's
// clipping/paint boundaries; work is already visible in activity and avatars.
function rowHtml(row) {
  const { kind, id, profile, activity } = row;
  const { working, text: title, tone } = sidebarActivity(row), name = displayName(row, profile);
  return `<article class="company-row ${sameItem(state.selected,row) ? "selected" : ""} ${working ? "working" : ""} ${isArchived(profile) ? "archived" : ""}" style="--agent:${escapeHtml(profileColor(profile))}" data-kind="${kind}" data-id="${escapeHtml(id)}" data-activity-tone="${tone}" draggable="true" role="button" tabindex="0" aria-label="Open ${escapeHtml(name)}. ${escapeHtml(title)}${activity?.unread ? '. Unread' : ''}" title="${escapeHtml(name)} — ${escapeHtml(title)}" ${sameItem(state.selected,row) ? 'aria-current="true"' : ""}>
    ${activity?.unread ? '<i class="unread-mark" aria-label="Unread"></i>' : ""}
    <span class="agent-avatar">${avatarSvg(profile,kind)}</span>
    <span class="row-copy">
      <span class="row-line"><strong class="row-name">${escapeHtml(name)}</strong>${profile.pinned ? `<i class="pin-glyph" title="Pinned">${ICONS.pin}</i>` : ""}<time class="row-time">${formatTime(activity?.modified_at)}</time></span>
      <span class="row-line"><span class="row-title ${working ? "activity" : ""}">${escapeHtml(title)}</span>${kind === "agent" ? providerIcon(activity?.provider_id, activity?.model) : ""}</span>
    </span>
    <span class="row-actions"><button class="row-more" type="button" aria-label="Options for ${escapeHtml(displayName(row,profile))}" aria-haspopup="menu">${ICONS.dots}</button></span>
  </article>`;
}

const boundCompanyRows=new WeakSet();
function reconcileNodes(parent,nodes){
  for(let i=0;i<nodes.length;i++)if(parent.children[i]!==nodes[i])parent.insertBefore(nodes[i],parent.children[i]||null);
  const keep=new Set(nodes);for(const child of [...parent.children])if(!keep.has(child))child.remove();
}
function sectionNode(name, rows, existing) {
  const node = existing || $("sectionTemplate").content.firstElementChild.cloneNode(true);
  node.dataset.section = name;
  node.querySelector(".section-title").textContent = name;
  const count=node.querySelector(".section-count");if(count.textContent!==String(rows.length))count.textContent=rows.length;
  const items = node.querySelector(".section-items"), toggle = node.querySelector(".section-toggle");
  const collapsed = state.collapsedSections.has(name);
  items.id = `sidebar-section-${slugId(name)}`;
  const previous=new Map([...items.children].map(row=>[itemKey(row.dataset.kind,row.dataset.id),row]));
  const next=rows.map(row=>{const html=rowHtml(row),old=previous.get(itemKey(row.kind,row.id));if(old?._phoenixRowMarkup===html)return old;const template=document.createElement("template");template.innerHTML=html;const fresh=template.content.firstElementChild;fresh._phoenixRowMarkup=html;return fresh;});
  reconcileNodes(items,next);
  node.classList.toggle("collapsed", collapsed);
  toggle.setAttribute("aria-controls", items.id);
  toggle.setAttribute("aria-expanded", String(!collapsed));
  toggle.setAttribute("aria-label", `${collapsed ? "Expand" : "Collapse"} ${name}`);
  if(!existing)toggle.addEventListener("click", () => {
    if (state.collapsedSections.has(name)) state.collapsedSections.delete(name); else state.collapsedSections.add(name);
    render();
  });
  return node;
}

function render() {
  if (!state.view) return;
  const list = $("sidebarList"), rows = allRows().filter(rowMatches);
  const scrollTop = list.scrollTop, focused = document.activeElement;
  const focusedRow = list.contains(focused) ? focused.closest(".company-row") : null;
  const focusedSection = list.contains(focused) ? focused.closest(".company-section")?.dataset.section : null;
  const focusSelector = focusedRow
    ? `.company-row[data-kind="${CSS.escape(focusedRow.dataset.kind)}"][data-id="${CSS.escape(focusedRow.dataset.id)}"]${focused.matches(".row-more") ? " .row-more" : ""}`
    : focusedSection ? `[data-section="${CSS.escape(focusedSection)}"] .section-toggle` : "";
  const sections=new Map([...list.querySelectorAll(":scope > .company-section")].map(node=>[node.dataset.section,node])),nextSections=[];
  const pinned = rows.filter((row) => row.profile.pinned);
  const groups = rows.filter((row) => !row.profile.pinned && row.kind === "group");
  const coworkers = rows.filter((row) => !row.profile.pinned && row.kind === "agent");
  if (pinned.length) nextSections.push(sectionNode("Pinned", pinned,sections.get("Pinned")));
  if (groups.length) nextSections.push(sectionNode("Groups", groups,sections.get("Groups")));
  if (coworkers.length) nextSections.push(sectionNode("Coworkers", coworkers,sections.get("Coworkers")));
  if(rows.length)reconcileNodes(list,nextSections);
  if (!rows.length) list.innerHTML = `<div class="empty-company"><strong>No one found</strong><span>Try another search or include archived coworkers.</span></div>`;
  bindRows();
  if (focusSelector) {
    const fallback = focusedSection && list.querySelector(`[data-section="${CSS.escape(focusedSection)}"] .section-toggle`);
    (list.querySelector(focusSelector) || fallback || $("companySearch"))?.focus({ preventScroll: true });
  }
  list.scrollTop = scrollTop;
  updateStage();
  $("filterButton").classList.toggle("active", state.showArchived || state.workingOnly || state.unreadOnly);
}

function bindRows() {
  document.querySelectorAll(".company-row").forEach((row) => {
    if(boundCompanyRows.has(row))return;boundCompanyRows.add(row);
    const item = wireItem(row.dataset.kind, row.dataset.id);
    row.addEventListener("click", (event) => {
      if (event.target.closest(".row-more")) return;
      selectItem(item);
    });
    row.addEventListener("keydown", (event) => { if (event.target === row && !event.defaultPrevented && ["Enter"," "].includes(event.key)) { event.preventDefault(); selectItem(item); } });
    row.querySelector(".row-more").addEventListener("click", (event) => { event.stopPropagation(); openRowMenu(event.currentTarget, item); });
    // Right-click opens the same options menu, in the full sidebar and in the
    // collapsed rail (where the ⋯ button is hidden).
    row.addEventListener("contextmenu", (event) => { event.preventDefault(); openRowMenu(row, item, { x: event.clientX, y: event.clientY }); });
    row.addEventListener("dragstart", (event) => { state.dragging = item; row.classList.add("dragging"); event.dataTransfer.effectAllowed = "move"; event.dataTransfer.setData("text/plain", itemKey(item.kind,item.id)); });
    row.addEventListener("dragend", () => { state.dragging = null; row.classList.remove("dragging"); document.querySelectorAll(".drag-over").forEach((el) => el.classList.remove("drag-over")); });
    row.addEventListener("dragover", (event) => { if (!state.dragging || sameItem(state.dragging,item)) return; event.preventDefault(); row.classList.add("drag-over"); });
    row.addEventListener("dragleave", () => row.classList.remove("drag-over"));
    row.addEventListener("drop", async (event) => { event.preventDefault(); row.classList.remove("drag-over"); await reorder(state.dragging, item); });
  });
}

function rememberSelection(){if(!TAURI||SIDEBAR_PREVIEW||!profileFor(state.selected))return;try{localStorage.setItem("phoenix-selected-conversation",JSON.stringify(state.selected))}catch{}}
function restoreSelection(){try{const item=JSON.parse(localStorage.getItem("phoenix-selected-conversation")||"null");if(item&&["agent","group"].includes(item.kind)&&typeof item.id==="string"&&profileFor(item)&&!isArchived(profileFor(item)))state.selected=item;}catch{}}
addEventListener("beforeunload",rememberSelection);
async function selectItem(item) {
  state.selected = item;
  rememberSelection();
  render();
  const activity = activityFor(item);
  const selected=new Promise((resolve)=>{
    const finish=(event)=>{if(!sameItem(event.detail?.item,item))return;removeEventListener("phoenix:conversation-selected",finish);clearTimeout(timer);resolve();};
    const timer=setTimeout(()=>{removeEventListener("phoenix:conversation-selected",finish);resolve();},8000);
    addEventListener("phoenix:conversation-selected",finish);
  });
  window.dispatchEvent(new CustomEvent("phoenix:select-conversation", { detail: { item, sessionId: activity?.canonical_session_id } }));
  if (narrowSidebarViewport()) toggleSidebar(true, false);
  await Promise.all([selected,activity?.unread?mutate({ action: "mark_read", item }, { quiet: true }):Promise.resolve()]);
}

function updateStage() {
  const profile = profileFor(state.selected);
  if (!profile) return;
  $("stageAvatar").classList.toggle("group-stage-avatar",state.selected.kind==="group");
  const stageAvatar=$("stageAvatar"),avatarMarkup=avatarSvg(profile,state.selected.kind);if(stageAvatar._phoenixAvatarMarkup!==avatarMarkup){stageAvatar.innerHTML=avatarMarkup;stageAvatar._phoenixAvatarMarkup=avatarMarkup;}
  const name=displayName(state.selected,profile);if($("stageName").textContent!==name)$("stageName").textContent=name;
  const members = state.selected.kind === "group" ? groupMembers(state.selected.id) : [];
  $("stageRole").textContent = state.selected.kind === "group" ? `${members.length} coworkers` : profile.role_title;
  let rosterButton = $("groupRosterButton");
  if (state.selected.kind !== "group") { rosterButton?.remove(); return; }
  if (!rosterButton) {
    rosterButton = document.createElement("button");
    rosterButton.id = "groupRosterButton";
    rosterButton.type = "button";
    rosterButton.className = "group-roster-trigger";
    rosterButton.setAttribute("aria-haspopup", "dialog");
    $("stageMore").before(rosterButton);
  }
  rosterButton.setAttribute("aria-label", `View ${displayName(state.selected,profile)} roster: ${members.length} coworkers`);
  rosterButton.innerHTML = `<span class="group-roster-stack" aria-hidden="true">${members.slice(0,3).map((member) => `<i style="--agent:${escapeHtml(profileColor(member))}">${avatarSvg(member)}</i>`).join("")}</span><small>${members.length}</small>`;
  rosterButton.onclick = () => openGroupRoster(rosterButton);
}

// Group leader: the chief of staff (agent_id "phoenix") leads by default
// when a member, else the first member. Snapshots carry the effective
// `leader_agent_id` on every group row.
const CHIEF_OF_STAFF_ID = "phoenix";
function defaultGroupLeader(memberIds) {
  return memberIds.includes(CHIEF_OF_STAFF_ID) ? CHIEF_OF_STAFF_ID : (memberIds[0] || null);
}
function groupLeaderId(groupId) {
  const group = state.view?.directory.groups.find((row) => row.group_id === groupId);
  const memberIds = (state.view?.directory.members || []).filter((member) => member.group_id === groupId).sort((a,b) => a.sort_order - b.sort_order).map((member) => member.agent_id);
  return group?.leader_agent_id && memberIds.includes(group.leader_agent_id) ? group.leader_agent_id : defaultGroupLeader(memberIds);
}

function groupMembers(groupId) {
  const agents = state.view?.directory.agents || [];
  return (state.view?.directory.members || [])
    .filter((member) => member.group_id === groupId)
    .sort((a,b) => a.sort_order - b.sort_order)
    .map((member) => agents.find((agent) => agent.agent_id === member.agent_id))
    // Membership is retained for history and restoration after deletion.
    // The current roster must match the coworkers who can join a new turn.
    .filter((agent) => agent?.lifecycle === "active");
}

function openGroupRoster(anchor) {
  if (state.selected.kind !== "group") return;
  const group = profileFor(state.selected), members = groupMembers(state.selected.id), leaderId = groupLeaderId(state.selected.id);
  const rows = members.map((member) => `<article class="group-roster-row${member.agent_id === leaderId ? " is-leader" : ""}" role="listitem">
    <i class="group-roster-avatar" style="--agent:${escapeHtml(profileColor(member))}">${avatarSvg(member)}</i>
    <span class="group-roster-copy"><strong>${escapeHtml(member.display_name)}${member.agent_id === leaderId ? ' <em class="group-leader-badge" title="Group leader: unaddressed messages go here">Leader</em>' : ""}</strong><small>${escapeHtml(member.role_title || "Coworker")}</small><p>${escapeHtml(member.description || "No responsibility description yet.")}</p></span>
  </article>`).join("");
  const pop = openPopover(anchor, `<div class="group-roster-head"><span><strong>${escapeHtml(group?.name || "Group")}</strong><small>${members.length} ${members.length === 1 ? "coworker" : "coworkers"}</small></span><button type="button" data-configure-roster aria-label="Configure group">${ICONS.edit}</button></div><div class="group-roster-list" role="list" aria-label="Group coworkers">${rows || '<p class="group-roster-empty">No coworkers in this group.</p>'}</div>`, "group-roster-popover", { align:"start" });
  pop.setAttribute("role", "dialog");
  pop.setAttribute("aria-label", `${group?.name || "Group"} roster`);
  pop.querySelector("[data-configure-roster]").onclick = () => { closeLayers(); openEditModal(state.selected); };
}

async function mutate(command, { quiet = false } = {}) {
  try { state.view = await directoryCommand(command); render(); window.dispatchEvent(new CustomEvent("phoenix:directory-updated",{detail:{view:state.view}})); if (!quiet) toast("Saved."); return true; }
  catch (error) { toast(error.message || String(error), true); return false; }
}

async function reorder(source, target) {
  if (!source || !target || sameItem(source,target)) return;
  const ordered = allRows().map((r) => wireItem(r.kind,r.id));
  const from = ordered.findIndex((item) => sameItem(item,source)), to = ordered.findIndex((item) => sameItem(item,target));
  if (from < 0 || to < 0) return;
  ordered.splice(to,0,ordered.splice(from,1)[0]);
  await mutate({ action: "reorder", items: ordered }, { quiet: true });
}

let popoverAnchor = null;
let modalReturnFocus = null;
let modalFocusRevision = 0;
function overlayControls(root) {
  return [...root.querySelectorAll('button,input:not([type="hidden"]),select,textarea,summary,a[href],[tabindex]')]
    .filter(node => !node.disabled && node.tabIndex >= 0 && node.getAttribute('aria-disabled') !== 'true'
      && node.getClientRects().length > 0 && getComputedStyle(node).visibility !== 'hidden');
}
function restoreOverlayFocus(target) {
  const current = target?.isConnected ? target : target?.id ? document.getElementById(target.id) : null;
  if (current && !current.disabled && current.getClientRects().length) current.focus({preventScroll:true});
}
function openPopover(anchor, html, className = "", options = {}) {
  closeLayers(false);
  popoverAnchor = anchor;
  const layer = $("menuLayer"); layer.hidden = false; layer.innerHTML = `<div class="popover ${className}" role="menu">${html}</div>`;
  const pop = layer.firstElementChild;
  placePopover(pop, anchor, options);
  choreographPopover(pop, anchor);
  queueMicrotask(() => {
    if (!pop.isConnected || layer.hidden || pop.contains(document.activeElement)) return;
    const nodes = overlayControls(pop);
    (nodes.find(node => node.matches('input[type="search"],.menu-select-search input'))
      || nodes.find(node => node.matches('.selected,[aria-selected="true"],[aria-checked="true"]')) || nodes[0])?.focus({preventScroll:true});
  });
  setTimeout(() => addEventListener("pointerdown", outsidePopover, { capture:true, once:true }),0);
  return pop;
}
function placePopover(pop, anchor, { align = "end", matchWidth = false, minWidth = 0 } = {}) {
  const rect = anchor.getBoundingClientRect();
  if (matchWidth) {
    const width = Math.max(160, Number(minWidth)||0, Math.round(rect.width));
    pop.style.width = `${width}px`;
    pop.style.minWidth = `${width}px`;
  }
  const width = pop.offsetWidth, height = pop.offsetHeight;
  let left = matchWidth || align === "start" ? rect.left : rect.right - width;
  let top = rect.bottom + 6;
  pop.dataset.placement = "bottom";
  if (top + height > innerHeight - 8 && rect.top - height - 6 >= 8) { top = rect.top - height - 6; pop.dataset.placement = "top"; }
  pop.style.left = `${Math.round(clamp(left, 8, Math.max(8, innerWidth - width - 8)))}px`;
  pop.style.top = `${Math.round(clamp(top, 8, Math.max(8, innerHeight - height - 8)))}px`;
}
// Every dropdown/drop-up follows beUI's Select: the panel starts flush on the
// trigger and pulls away on a bouncy gap, its near corners round as it
// separates, rows fade in one after another, and a select-like trigger's
// facing edge flattens then rounds while its chevron flips.
function choreographPopover(pop, anchor) {
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  const top = pop.dataset.placement === "top";
  pop.classList.add("select-panel", top ? "select-panel-top" : "select-panel-bottom");
  const rows = [...pop.querySelectorAll(":scope > button, :scope > label, :scope > a, .choice-row, :scope > div > button, :scope > div > label, .model-picker-list > *")].slice(0, 24);
  rows.forEach((row, index) => { row.classList.add("select-row-in"); row.style.animationDelay = `${50 + index * 35}ms`; });
  if (anchor && anchor.getBoundingClientRect().width >= 48) {
    anchor.classList.remove("select-trigger-open", "select-trigger-top", "select-trigger-bottom");
    void anchor.offsetWidth;
    anchor.classList.add("select-trigger-open", top ? "select-trigger-top" : "select-trigger-bottom");
  }
}
function outsidePopover(event) { if (!event.target.closest(".popover")) closeLayers(false); else addEventListener("pointerdown", outsidePopover, { capture:true, once:true }); }
function closeLayers(restoreFocus = true) {
  const layer = $("menuLayer"), wasOpen = !layer.hidden, anchor = popoverAnchor;
  layer.hidden = true; layer.innerHTML = ""; popoverAnchor = null;
  document.querySelectorAll(".menu-open").forEach((el) => el.classList.remove("menu-open"));
  document.querySelectorAll(".select-trigger-open").forEach((el) => el.classList.remove("select-trigger-open", "select-trigger-top", "select-trigger-bottom"));
  if (wasOpen && restoreFocus === true) restoreOverlayFocus(anchor);
}

function openRowMenu(anchor, item, point = null) {
  const profile = profileFor(item), row = anchor.closest(".company-row");
  // A right-click menu opens at the pointer rather than under the row.
  if (point) { const at = { left: point.x, right: point.x, top: point.y, bottom: point.y, width: 0, height: 0, x: point.x, y: point.y }; anchor = { getBoundingClientRect: () => at, closest: (selector) => row?.closest(selector) || null, contains: () => false, focus: () => row?.focus?.({ preventScroll: true }), getClientRects: () => row?.getClientRects() || [], disabled: false, classList: row?.classList, isConnected: Boolean(row?.isConnected), id: "" }; }
  const archived = isArchived(profile), pinLabel = profile.pinned ? "Unpin" : "Pin to top";
  const html = `<div class="popover-label">${escapeHtml(displayName(item,profile))}</div>
    <button data-action="pin">${ICONS.pin}<span>${pinLabel}</span></button>
    <button data-action="edit">${ICONS.edit}<span>Configure</span></button>
    ${item.kind==="agent"&&item.id!=="phoenix"?`<button data-action="clone">${ICONS.copy}<span>Duplicate coworker</span></button>`:""}
    <button data-action="archive">${ICONS.archive}<span>${archived ? "Restore" : "Archive"}</span></button>
    <div class="popover-separator"></div>
    <button class="danger" data-action="delete">${ICONS.trash}<span>Delete…</span><small>30 days</small></button>`;
  const pop = openPopover(anchor,html,"",point?{align:"start"}:{});row?.classList.add("menu-open");
  pop.addEventListener("click", async (event) => {
    const action = event.target.closest("button")?.dataset.action; if (!action) return; closeLayers();
    if (action === "pin") await mutate({ action:"set_pinned", item, pinned:!profile.pinned });
    if (action === "archive") await mutate({ action:item.kind === "agent" ? "set_agent_lifecycle" : "set_group_lifecycle", [`${item.kind}_id`]:item.id, lifecycle:archived ? "active" : "archived" });
    if (action === "edit") openEditModal(item);
    if(action==="clone"){
      const name=`${profile.display_name} Copy`;
      if(await mutate({action:"clone_agent",source_agent_id:item.id,preferred_name:name},{quiet:true}))toast(`${name} created · Phoenix is setting up the new role.`);
    }
    if (action === "delete") openDeleteModal(item);
  });
}

function openFilter() {
  const row = (key, label, on) => `<label class="ember-check-label"><input class="ember-check-input" data-filter="${key}" type="checkbox" ${on ? "checked" : ""}><i class="ember-check-box" aria-hidden="true"></i><span>${label}</span></label>`;
  const html = `<div class="popover-label">Show</div>
    ${row("showArchived", "Archived & deleting", state.showArchived)}
    ${row("workingOnly", "Working now only", state.workingOnly)}
    ${row("unreadOnly", "Unread only", state.unreadOnly)}`;
  const pop = openPopover($("filterButton"),html,"filter-popover");
  pop.querySelectorAll("input").forEach((input) => input.addEventListener("change", () => { state[input.dataset.filter] = input.checked; render(); }));
}

// The corner button is a sun/moon theme switch (beUI ThemeToggle, blinds).
let themeSaveChain = Promise.resolve();
async function toggleThemeFromButton() {
  // The rendered palette lags during blinds. Toggle the selected preference,
  // so another ordinary click does not re-save the same stale color theme.
  const theme = resolvedTheme() === "dark" ? "light" : "dark";
  setTheme(theme);
  try { themeSaveChain = themeSaveChain.catch(() => {}).then(() => window.PhoenixSettings?.saveAppearance("appearance.theme", theme)); await themeSaveChain; }
  catch (error) { toast(`Could not save appearance: ${error.message || error}`, true); }
}
function syncThemeToggle() {
  const button = $("profileButton"); if (!button) return;
  const dark = document.documentElement.dataset.theme === "dark";
  button.dataset.mode = dark ? "dark" : "light";
  button.setAttribute("aria-label", dark ? "Switch to light mode" : "Switch to dark mode");
  button.title = dark ? "Switch to light mode" : "Switch to dark mode";
}

// Popovers now sit ABOVE the modal layer, so a menu left open from the surface
// underneath would float over the new modal. Opening a modal dismisses it.
function showModal(html) {
  // A closing sidebar snapshot sits above ordinary content. Retire it before
  // exposing the modal, including when Configure interrupts a resize.
  stopSidebarTransition();
  const layer = $("modalLayer"), active = document.activeElement;
  if (layer.hidden || !layer.contains(active)) modalReturnFocus = popoverAnchor || active;
  closeLayers(false);
  const revision = ++modalFocusRevision;
  layer.hidden = false; layer.innerHTML = html;
  window.dispatchEvent(new CustomEvent("phoenix:modal-visibility",{detail:{open:true}}));
  layer.querySelector(".modal-close")?.addEventListener("click",closeModal);
  layer.onpointerdown = event => { if (event.target === layer) closeModal(); };
  queueMicrotask(() => {
    // Individual dialogs may deliberately focus a heading or a particular
    // field synchronously. Preserve that decision, including channel recovery.
    if (revision !== modalFocusRevision || layer.hidden || layer.contains(document.activeElement) || !$("menuLayer").hidden) return;
    const nodes = overlayControls(layer);
    const target = nodes.find(node => node.hasAttribute('autofocus')) || nodes.find(node => node.matches('input,textarea,select')) || nodes[0] || layer.firstElementChild;
    if (target && !nodes.includes(target)) target.tabIndex = -1;
    target?.focus({preventScroll:true});
  });
}
function closeModal(reason="dismiss") {
  const layer=$("modalLayer");if(layer.hidden)return;
  const normalized=typeof reason==="string"?reason:"dismiss", target=modalReturnFocus;
  ++modalFocusRevision;modalReturnFocus=null;
  closeLayers(false);
  layer.dispatchEvent(new CustomEvent("phoenix:modal-closing",{detail:{reason:normalized}}));
  layer.hidden = true; layer.innerHTML = "";
  queueMicrotask(() => {
    // A dialog-specific restore or newly opened dialog takes precedence.
    if (layer.hidden && $("menuLayer").hidden && (document.activeElement === document.body || !document.activeElement?.isConnected)) restoreOverlayFocus(target);
    window.dispatchEvent(new CustomEvent("phoenix:modal-visibility",{detail:{open:!layer.hidden,reason:normalized}}));
  });
}
function handleOverlayKeydown(event) {
  if (event.defaultPrevented) return;
  const menu = $("menuLayer"), modal = $("modalLayer");
  if (!menu.hidden && ['ArrowDown','ArrowUp','Home','End'].includes(event.key)) {
    const active = document.activeElement;
    // Keep ordinary caret movement in the search field; vertical arrows move
    // through the menu's actual focusable choices, without selecting a value.
    if (['Home','End'].includes(event.key) && active?.matches('input,textarea,[contenteditable="true"]')) return;
    const nodes = overlayControls(menu);
    if (nodes.length) {
      const at = nodes.indexOf(active);
      const next = event.key === 'Home' ? 0 : event.key === 'End' ? nodes.length - 1
        : at < 0 ? (event.key === 'ArrowDown' ? 0 : nodes.length - 1)
        : (at + (event.key === 'ArrowDown' ? 1 : -1) + nodes.length) % nodes.length;
      event.preventDefault();nodes[next].focus({preventScroll:true});
      nodes[next].scrollIntoView?.({block:'nearest'});
    }
    return;
  }
  if (event.key === 'Escape' && (!menu.hidden || !modal.hidden)) {
    event.preventDefault();event.stopImmediatePropagation();
    if (!menu.hidden) closeLayers(); else closeModal();
    return;
  }
  if (event.key !== 'Tab') return;
  if (!menu.hidden) closeLayers();
  if (modal.hidden) return;
  const nodes = overlayControls(modal), active = document.activeElement;
  const first = nodes[0], last = nodes.at(-1);
  if (!nodes.includes(active) || (event.shiftKey ? active === first : active === last)) {
    event.preventDefault();(event.shiftKey ? last : first)?.focus({preventScroll:true});
  }
}

function activeCoworkers() {
  return (state.view?.directory.agents || []).filter((agent) => agent.kind === "responsibility_owner" && agent.lifecycle === "active");
}

function suggestedGroupName(memberIds, description = "") {
  const names = memberIds.map((id) => activeCoworkers().find((agent) => agent.agent_id === id)?.display_name).filter(Boolean);
  if (names.length === 2) return `${names[0]} + ${names[1]}`;
  if (names.length === 3) return `${names[0]}, ${names[1]} + ${names[2]}`;
  if (names.length > 3) return `${names[0]}, ${names[1]} + ${names.length - 2}`;
  if (names.length === 1) return `${names[0]} Team`;
  const words = String(description || "").trim().split(/\s+/).map((word) => word.replace(/[^a-z0-9'-]/gi, "")).filter(Boolean).slice(0,3);
  if (words.length) return `${words.map((word) => word[0].toUpperCase() + word.slice(1)).join(" ")} Team`;
  return "New group";
}

function groupMemberPicker(agents, selectedIds = [], leaderId = null) {
  const selected = new Set(selectedIds);
  const rows = agents.map((agent) => `<label class="member-picker-row">
    <input class="ember-check-input" type="checkbox" name="members" value="${escapeHtml(agent.agent_id)}" ${selected.has(agent.agent_id) ? "checked" : ""}>
    <i class="ember-check-box" aria-hidden="true"></i>
    <i class="mini-avatar" style="--agent:${escapeHtml(profileColor(agent))}">${avatarSvg(agent)}</i>
    <span class="member-picker-copy"><strong>${escapeHtml(agent.display_name)}</strong><small>${escapeHtml(agent.role_title || "Coworker")}</small><p>${escapeHtml(agent.description || "No responsibility description yet.")}</p></span>
  </label>`).join("");
  return `<fieldset class="member-field"><legend>Coworkers</legend><div class="member-picker" aria-describedby="memberPickerStatus">${rows || '<p class="member-picker-empty">Create or activate at least two coworkers before making a group.</p>'}</div><span id="memberPickerStatus" class="member-picker-status" data-member-count aria-live="polite">Choose 2–6 active coworkers.</span></fieldset>
  <label class="field group-leader-field"><span>Leader <small>gets every message without an @mention and coordinates the room</small></span><select name="leader" data-group-leader data-initial-leader="${escapeHtml(leaderId || "")}"></select></label>`;
}

function bindGroupMemberPicker(form, { autoName = false, unchangedMemberIds = null } = {}) {
  const inputs = [...form.querySelectorAll('input[name="members"]')], counter = form.querySelector("[data-member-count]"), nameInput = form.elements.name, description = form.elements.description;
  const submit = $("modalLayer").querySelector(`[form="${form.id}"][type="submit"]`);
  if (autoName && nameInput) nameInput.dataset.autoGroupName = "true";
  const update = () => {
    const selectedIds = inputs.filter((input) => input.checked).map((input) => input.value), count = selectedIds.length;
    const unchanged = Array.isArray(unchangedMemberIds) && sameStringSet(selectedIds, unchangedMemberIds);
    const valid = unchanged || (count >= 2 && count <= 6);
    inputs.forEach((input) => { input.disabled = !input.checked && count >= 6; });
    if (counter) {
      counter.textContent = unchanged ? `${count} active coworkers · current roster preserved` : valid ? `${count} coworkers selected` : count < 2 ? `${count} selected · choose at least ${2 - count} more` : `${count} selected · remove ${count - 6}`;
      counter.classList.toggle("valid", valid);
      counter.classList.toggle("invalid", !valid);
    }
    if (submit) submit.disabled = !valid;
    const suggestion = suggestedGroupName(selectedIds, description?.value);
    form.dataset.groupSuggestion = suggestion;
    const hint = form.querySelector("[data-group-name-hint]");
    if (hint) hint.textContent = `Suggested from this roster: ${suggestion}`;
    if (autoName && nameInput?.dataset.autoGroupName === "true") nameInput.value = suggestion;
    const leaderSelect = form.querySelector("[data-group-leader]");
    if (leaderSelect) {
      const wanted = leaderSelect.value || leaderSelect.dataset.initialLeader || "";
      const chosen = selectedIds.includes(wanted) ? wanted : defaultGroupLeader(selectedIds);
      leaderSelect.innerHTML = selectedIds.map((id) => {
        const agent = activeCoworkers().find((row) => row.agent_id === id);
        const label = `${agent?.display_name || id}${id === CHIEF_OF_STAFF_ID ? " · chief of staff" : ""}`;
        return `<option value="${escapeHtml(id)}" ${id === chosen ? "selected" : ""}>${escapeHtml(label)}</option>`;
      }).join("");
      leaderSelect.disabled = !selectedIds.length;
    }
  };
  inputs.forEach((input) => input.addEventListener("change", update));
  description?.addEventListener("input", update);
  if (autoName && nameInput) nameInput.addEventListener("input", () => { nameInput.dataset.autoGroupName = nameInput.value.trim() ? "false" : "true"; update(); });
  update();
  return () => inputs.filter((input) => input.checked).map((input) => input.value);
}

function groupMemberPayload(memberIds) {
  return memberIds.map((agent_id) => ({ agent_id, member_role:"member", history_access:"full" }));
}

function openCreateModal(tab = "agent") {
  const agents = activeCoworkers();
  showModal(`<section class="modal" role="dialog" aria-modal="true" aria-labelledby="createTitle">
    <header class="modal-header"><span><strong id="createTitle">Grow your company</strong><small>${window.__PHOENIX_ISOLATED_BACKEND__?.enabled ? 'Save a coworker request, or create a shared room. Role setup needs an authorized subscription.' : 'A useful coworker in about a minute, or a room where several coworkers can think together.'}</small></span><button class="modal-close" aria-label="Close"><svg viewBox="0 0 20 20"><path d="m5 5 10 10M15 5 5 15"/></svg></button></header>
    <nav class="modal-tabs"><button data-tab="agent" class="${tab === "agent" ? "active" : ""}">Coworker</button><button data-tab="group" class="${tab === "group" ? "active" : ""}">Group</button></nav>
    <form id="createForm" class="modal-body" data-tab="${tab}">${tab === "agent" ? `
      <label class="field"><span>What should they own?</span><textarea name="description" rows="4" required autofocus placeholder="Manage my inbox, draft replies in my voice, and make sure important messages never get dropped."></textarea></label>
      <div class="field-row"><label class="field"><span>Name <small>optional</small></span><input name="name" placeholder="Phoenix can choose"></label></div>${avatarEditor({agent_id:"new-coworker",color:"#e55732",icon_seed:"new",metadata_json:"{}"})}` : `
      <label class="field group-name-field"><span>Group name <small>generated · editable</small></span><input name="name" maxlength="72" autofocus aria-describedby="groupNameHint"><small id="groupNameHint" class="field-hint" data-group-name-hint>Choose coworkers for a live suggestion.</small></label>
      <label class="field"><span>What is this group for?</span><textarea name="description" rows="3" required placeholder="Make product launch decisions together."></textarea></label>
      ${groupMemberPicker(agents)}
      <label class="field"><span>Color</span>${colorField("color","#e55732")}</label>`}</form>
    <footer class="modal-footer"><button class="button secondary" data-cancel>Cancel</button><button class="button primary" form="createForm" type="submit">${tab === "agent" ? "Create coworker" : "Create group"}</button></footer>
  </section>`);
  document.querySelectorAll(".modal-tabs button").forEach((button) => button.onclick = () => openCreateModal(button.dataset.tab));
  bindColorField($("modalLayer"));
  if (tab === "agent") bindAvatarEditor($("createForm"));
  const selectedGroupMembers = tab === "group" ? bindGroupMemberPicker($("createForm"), { autoName:true }) : null;
  $("modalLayer").querySelector("[data-cancel]").onclick = closeModal;
  $("createForm").addEventListener("submit", async (event) => {
    event.preventDefault(); const form=event.currentTarget, data = new FormData(form), submit = $("modalLayer").querySelector("[type=submit]"); submit.disabled = true;
    try {
      const members = selectedGroupMembers?.() || [];
      if (tab === "group" && (members.length < 2 || members.length > 6)) throw new Error("Choose between 2 and 6 active coworkers.");
      const groupName = String(data.get("name") || "").trim() || event.currentTarget.dataset.groupSuggestion || suggestedGroupName(members,data.get("description"));
      const command = tab === "agent" ? { action:"create_agent", description:data.get("description"), preferred_name:data.get("name") || null, color:data.get("color") || avatarAccentColor(event.currentTarget) || "#e55732", avatar:await avatarConfigFromForm(event.currentTarget) } : { action:"create_group", name:groupName, description:data.get("description"), color:data.get("color"), icon_seed:slugId(groupName), members, settings:{ discussion_rounds:1, read_full_transcript:true }, leader_agent_id:(members.includes(data.get("leader")) ? data.get("leader") : defaultGroupLeader(members)) };
      const oldIds=new Set(state.view.directory.agents.map(a=>a.agent_id)),avatarDraft=tab==='agent'?avatarFormValues(form):null;
      if (await mutate(command,{quiet:true})) {
        if(tab==='agent'){
          const created=state.view.directory.agents.find(a=>!oldIds.has(a.agent_id));
          if(created&&avatarDraft?.mode==='fluffy'){window.PhoenixAvatarPreferences.set(created.agent_id,'fluffy',avatarDraft.fluffy_palette,avatarDraft.fluffy_shape);render();window.dispatchEvent(new CustomEvent('phoenix:directory-updated',{detail:{view:state.view}}));}
        }
        closeModal();toast(tab==='agent' ? (window.__PHOENIX_ISOLATED_BACKEND__?.enabled?'Coworker request saved. Role setup needs an authorized subscription.':'Phoenix is setting up your new coworker.') : 'Group created.');
      } else submit.disabled = false;
    } catch (error) { toast(error.message || String(error), true); submit.disabled = false; }
  });
}

function openEditModal(item) {
  const p = profileFor(item), agent = item.kind === "agent", agents = activeCoworkers();
  const currentMemberIds = agent ? [] : groupMembers(item.id).map((member) => member.agent_id);
  const activeAgentIds = new Set(agents.map((coworker) => coworker.agent_id));
  const currentActiveMemberIds = currentMemberIds.filter((agentId) => activeAgentIds.has(agentId));
  const currentLeaderId = agent ? null : groupLeaderId(item.id);
  showModal(`<section class="modal" role="dialog" aria-modal="true"><header class="modal-header"><span><strong>Configure ${escapeHtml(displayName(item,p))}</strong><small>The canonical thread, memory, browser profile, and shared company knowledge stay intact.</small></span><button class="modal-close" aria-label="Close"><svg viewBox="0 0 20 20"><path d="m5 5 10 10M15 5 5 15"/></svg></button></header>
    <form id="editForm" class="modal-body"><label class="field"><span>Name</span><input name="name" required maxlength="72" value="${escapeHtml(displayName(item,p))}"></label>${agent ? avatarEditor(p) : `<label class="field"><span>Color</span>${colorField("color",profileColor(p))}</label>`}${agent ? `<label class="field"><span>Responsibility</span><input name="role" required value="${escapeHtml(p.role_title)}"></label>` : ""}<label class="field"><span>Description</span><textarea name="description" rows="4">${escapeHtml(p.description)}</textarea></label>${agent ? "" : groupMemberPicker(agents,currentMemberIds,currentLeaderId)}</form>
    <footer class="modal-footer"><button class="button secondary" data-cancel>Cancel</button><button class="button primary" form="editForm" type="submit">Save changes</button></footer></section>`);
  $('editForm').dataset.configureKind=item.kind;$('editForm').dataset.configureId=item.id;
  bindColorField($("modalLayer"));
  if (agent) bindAvatarEditor($("editForm"),p);
  const selectedGroupMembers = agent ? null : bindGroupMemberPicker($("editForm"), { unchangedMemberIds: currentActiveMemberIds });
  $("modalLayer").querySelector("[data-cancel]").onclick = closeModal;
  $("editForm").onsubmit = async (event) => {
    event.preventDefault();
    const form = event.currentTarget, d = new FormData(form), submit = $("modalLayer").querySelector('[form="editForm"][type="submit"]');
    submit.disabled = true;
    try {
      const members = selectedGroupMembers?.() || [];
      const rosterChanged = !agent && !sameStringSet(members, currentActiveMemberIds);
      if (rosterChanged && (members.length < 2 || members.length > 6)) throw new Error("Choose between 2 and 6 active coworkers.");
      const command = agent ? { action:"update_agent", agent_id:item.id, display_name:d.get("name"), role_title:d.get("role"), description:d.get("description"), color:d.get("color") || avatarAccentColor(event.currentTarget) || profileColor(p), icon_seed:p.icon_seed, avatar:await avatarConfigFromForm(event.currentTarget) } : { action:"update_group", group_id:item.id, name:d.get("name"), description:d.get("description"), color:d.get("color"), icon_seed:p.icon_seed, settings:null };
      if (agent && command.avatar === undefined) delete command.avatar;
      if (agent && window.__PHOENIX_ISOLATED_BACKEND__?.current===true) {
        // Send only real changes. Unchanged behavioral fields must not turn an
        // avatar-only Save into a role/prompt edit. Changed protected fields
        // remain in the payload so the main-process policy denies the whole write.
        for (const key of ["display_name","role_title","description","color","icon_seed"]) if(command[key]===p[key])delete command[key];
        if(command.avatar&&JSON.stringify(command.avatar)===JSON.stringify(JSON.parse(p.metadata_json||"{}").avatar))delete command.avatar;
        if(Object.keys(command).length===2){
          window.PhoenixAvatarPreferences.commit(item.id,form);render();window.dispatchEvent(new CustomEvent("phoenix:directory-updated",{detail:{view:state.view}}));closeModal();toast("Configuration saved.");return;
        }
      }
      if (!await mutate(command,{quiet:true})) { submit.disabled = false; return; }
      if (agent) { window.PhoenixAvatarPreferences.commit(item.id,form); render();window.dispatchEvent(new CustomEvent("phoenix:directory-updated",{detail:{view:state.view}})); }
      if (rosterChanged && !await mutate({ action:"set_group_members", group_id:item.id, members:groupMemberPayload(members) }, { quiet:true })) { submit.disabled = false; return; }
      const chosenLeader = agent ? null : d.get("leader");
      if (chosenLeader && members.includes(chosenLeader) && chosenLeader !== groupLeaderId(item.id) && !await mutate({ action:"set_group_leader", group_id:item.id, leader_agent_id:chosenLeader }, { quiet:true })) { submit.disabled = false; return; }
      closeModal(); toast("Configuration saved.");
    } catch (error) { toast(error.message || String(error),true); submit.disabled = false; }
  };
}

function openDeleteModal(item) {
  const p = profileFor(item), name = displayName(item,p);
  showModal(`<section class="modal" role="dialog" aria-modal="true"><header class="modal-header"><span><strong>Delete ${escapeHtml(name)}?</strong><small>This starts a 30-day recovery period. Shared knowledge and shared workflows remain with the company.</small></span><button class="modal-close" aria-label="Close"><svg viewBox="0 0 20 20"><path d="m5 5 10 10M15 5 5 15"/></svg></button></header><form id="deleteForm" class="modal-body"><p class="confirm-copy">Type <strong>${escapeHtml(name)}</strong> to confirm. The ${item.kind} can be restored during the next 30 days.</p><label class="field"><span>Exact name</span><input name="confirm" autocomplete="off" required></label></form><footer class="modal-footer"><button class="button secondary" data-cancel>Cancel</button><button class="button danger" form="deleteForm" type="submit" disabled>Start 30-day deletion</button></footer></section>`);
  const input = $("deleteForm").elements.confirm, submit = $("modalLayer").querySelector("[type=submit]"); input.oninput = () => submit.disabled = input.value !== name; $("modalLayer").querySelector("[data-cancel]").onclick = closeModal;
  $("deleteForm").onsubmit = async (event) => { event.preventDefault(); const command = { action:item.kind === "agent" ? "schedule_agent_deletion" : "schedule_group_deletion", [`${item.kind}_id`]:item.id, confirmed_name:name }; if (await mutate(command,{quiet:true})) { closeModal(); toast(`${name} can be recovered for 30 days.`); } };
}

function desktopWindow() {
  const native = window.__TAURI__?.window?.getCurrentWindow?.()
    || window.__TAURI__?.webviewWindow?.getCurrentWebviewWindow?.();
  if (native) return native;
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) return null;
  return {
    minimize: () => invoke("plugin:window|minimize"),
    toggleMaximize: () => invoke("plugin:window|toggle_maximize"),
    close: () => invoke("plugin:window|close"),
    setTheme: (theme) => invoke("plugin:window|set_theme", { theme }),
    setBackgroundColor: (color) => invoke("plugin:window|set_background_color", { color }),
  };
}
function resolvedTheme(theme = localStorage.getItem("phoenix-theme") || "light") {
  if (theme === "system") return matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  return theme === "dark" ? "dark" : "light";
}
// A real change of palette plays beUI's "blinds" view transition: slats
// sweep across the window revealing the new theme (agent-components.css).
function setTheme(theme) {
  const choice = theme === "system" ? "system" : theme === "dark" ? "dark" : "light";
  const root = document.documentElement, current = root.dataset.theme, next = resolvedTheme(choice);
  // A newer saved choice supersedes an unfinished visual transition. Removing
  // its node also prevents the old fallback timer from applying a stale theme.
  document.querySelectorAll(".theme-blinds").forEach(node => node.remove());
  const animate = Boolean(current) && current !== next
    && !matchMedia("(prefers-reduced-motion: reduce)").matches && document.visibilityState === "visible";
  if (!animate) { applyTheme(choice); return; }
  // Record the choice now so pickers painted during the blinds show it.
  localStorage.setItem("phoenix-theme", choice);
  root.dataset.themeChoice = choice;
  // View Transitions stall under Electron's Vulkan/Wayland compositor, so the
  // blinds are drawn directly: slats in the new theme's colour sweep shut
  // across the window, the theme swaps underneath, then the slats open to
  // reveal it (masked 72px tiles, like beUI's blinds).
  document.querySelector(".theme-blinds")?.remove();
  const blinds = document.createElement("div");
  blinds.className = "theme-blinds";
  blinds.style.setProperty("--blinds-color", next === "dark" ? "#18191d" : "#f8f7f4");
  document.body.append(blinds);
  blinds.addEventListener("animationend", (event) => {
    if (event.animationName === "theme-blinds-close") { applyTheme(choice); blinds.classList.add("opening"); }
    else if (event.animationName === "theme-blinds-open") blinds.remove();
  });
  setTimeout(() => { if (blinds.isConnected) { if (root.dataset.theme !== next) applyTheme(choice); blinds.remove(); } }, 1600);
}
// Interface zoom from inside the page (the shell also catches real keys
// first; whichever sees the key handles it). Ctrl + scroll zooms too.
addEventListener("keydown", (event) => {
  if (!(event.ctrlKey || event.metaKey) || event.altKey) return;
  const step = event.key === "=" || event.key === "+" ? 1 : event.key === "-" || event.key === "_" ? -1 : event.key === "0" && !event.shiftKey ? 0 : null;
  if (step === null) return;
  event.preventDefault();
  window.PhoenixUI?.invoke?.("app_zoom", { step }).then(showZoomLevel).catch(() => {});
}, true);
// A small "110%" pill so the user can see how far they are zoomed.
function showZoomLevel(factor) {
  if (!Number.isFinite(factor)) return;
  let pill = document.getElementById("zoomLevel");
  if (!pill) { pill = document.createElement("div"); pill.id = "zoomLevel"; pill.className = "zoom-level"; pill.setAttribute("role", "status"); document.body.append(pill); }
  pill.textContent = `${Math.round(factor * 100)}%`;
  pill.classList.add("visible");
  clearTimeout(showZoomLevel.timer);
  showZoomLevel.timer = setTimeout(() => pill.classList.remove("visible"), 1200);
}
window.__TAURI__?.event?.listen?.("app-zoom", (event) => showZoomLevel(Number(event.payload?.factor)));
// A clicked notification opens its conversation.
window.__TAURI__?.event?.listen?.("open-conversation", (event) => {
  const kind = String(event.payload?.kind || ""), id = String(event.payload?.id || "");
  if (!kind || !id) return;
  const known = (state.view?.activities || []).find((row) => row.item?.kind === kind && row.item?.id === id)?.item;
  selectItem(known || { kind, id });
});
let zoomWheelAt = 0;
addEventListener("wheel", (event) => {
  if (!event.ctrlKey) return;
  event.preventDefault();
  const now = Date.now(); if (now - zoomWheelAt < 120) return; zoomWheelAt = now;
  window.PhoenixUI?.invoke?.("app_zoom", { step: event.deltaY < 0 ? 1 : -1 }).then(showZoomLevel).catch(() => {});
}, { passive: false, capture: true });
function applyTheme(choice) {
  localStorage.setItem("phoenix-theme", choice);
  const resolved = resolvedTheme(choice);
  document.documentElement.dataset.theme = resolved;
  document.documentElement.dataset.themeChoice = choice;
  dispatchEvent(new CustomEvent("phoenix:theme-changed", { detail: { choice, resolved } }));
  syncThemeToggle();
  const win = desktopWindow();
  if (!win) return;
  // Match the native window fill to the actual palette rather than flashing
  // an unrelated black/white surface while the renderer paints or resizes.
  const color = getComputedStyle(document.documentElement).getPropertyValue("--bg").trim()
    || (resolved === "dark" ? "#18191d" : "#f8f7f4");
  win.setTheme?.(resolved)?.catch?.(() => {});
  // The soft sky has a transparent --bg; soft-sky.js sends the sky colour
  // instead, so never hand the native window a colour it cannot parse.
  if (/^(transparent|rgba\(0, 0, 0, 0\))$/.test(color)) return;
  win.setBackgroundColor?.(color)?.catch?.(() => {});
}
const VISUAL_DEFAULTS = Object.freeze({ conversationView:"compact", density:"comfortable", radius:"soft", fire:"full", contrast:"standard", feed:"regular", accent:"ember", stageFire:"full", flicker:"on", rail:"bold", type:"regular", glow:"ember", conversationText:"default", conversationWidth:"default" });
function visualPrefs() {
  try { return { ...VISUAL_DEFAULTS, ...JSON.parse(localStorage.getItem("phoenix-visual") || "{}") }; }
  catch { return { ...VISUAL_DEFAULTS }; }
}
function applyVisualPrefs(next = visualPrefs()) {
  const prefs = { ...VISUAL_DEFAULTS, ...next };
  prefs.conversationText = ["default","small","smaller"].includes(prefs.conversationText) ? prefs.conversationText : "default";
  prefs.conversationWidth = prefs.conversationWidth === "extra" ? "full" : ["default","full"].includes(prefs.conversationWidth) ? prefs.conversationWidth : "default";
  localStorage.setItem("phoenix-visual", JSON.stringify(prefs));
  const root = document.documentElement;
  // Compact keeps the thread scannable; detailed unfolds the agent's edits
  // as real code. The feed reads this off the root, so it flips live.
  root.dataset.conversationView = prefs.conversationView;
  root.dataset.cursorMotion = ["signature_arc","spring_settle","magnetic","comet_swoop","adaptive","classic"].includes(prefs.cursorMotion) ? prefs.cursorMotion : "signature_arc";
  root.dataset.density = prefs.density;
  root.dataset.radius = prefs.radius;
  root.dataset.fire = prefs.fire;
  root.dataset.contrast = prefs.contrast;
  root.dataset.feed = prefs.feed;
  root.dataset.accent = prefs.accent;
  root.dataset.stageFire = prefs.stageFire;
  root.dataset.flicker = prefs.flicker;
  root.dataset.rail = prefs.rail;
  root.dataset.type = prefs.type;
  root.dataset.glow = prefs.glow;
  root.dataset.conversationText = prefs.conversationText;
  root.dataset.conversationWidth = prefs.conversationWidth;
  // These widths and box sizes are written inline, so they beat the CSS
  // tokens. They have to carry --interface-scale themselves or the appearance
  // control silently does nothing to them.
  const feeds = { narrow:"calc(600px * var(--interface-scale))", regular:"calc(700px * var(--interface-scale))", wide:"calc(900px * var(--interface-scale))" };
  const radii = { sharp:4, soft:10, round:18 };
  const radius = radii[prefs.radius] ?? 10;
  const compact = prefs.density === "compact";
  root.style.setProperty("--feed-max", feeds[prefs.feed] || feeds.regular);
  root.style.setProperty("--radius", `${radius}px`);
  root.style.setProperty("--radius-sm", `${Math.max(3, radius - 3)}px`);
  root.style.setProperty("--avatar", compact ? "calc(21px * var(--interface-scale))" : "calc(24px * var(--interface-scale))");
  root.style.setProperty("--row-h", compact ? "calc(36px * var(--interface-scale))" : "calc(42px * var(--interface-scale))");
  window.dispatchEvent(new CustomEvent("phoenix:visual-prefs-changed",{detail:prefs}));
}
function bindWindowChrome() {
  const own = window.PhoenixPreviewWindow;
  const win = own ? { minimize:own.minimize, toggleMaximize:own.maximize, close:own.close } : desktopWindow();
  const run=async(action,label)=>{
    if(!win?.[action]){toast(`${label} is unavailable in this window.`,true);return;}
    try{await win[action]();}catch(error){toast(`${label} failed: ${error.message||error}`,true);}
  };
  $("winMin")?.addEventListener("click", () => run("minimize","Minimize"));
  $("winMax")?.addEventListener("click", () => run("toggleMaximize","Maximize"));
  $("winClose")?.addEventListener("click", () => run("close","Close"));
}
function narrowSidebarViewport() { return window.matchMedia?.("(max-width: 560px)")?.matches ?? false; }
let sidebarTransitionGeneration=0,sidebarTransitionAnimations=[],sidebarTransitionSnapshot=null,sidebarTransitionTimer=0;
function stopSidebarTransition(){clearTimeout(sidebarTransitionTimer);sidebarTransitionTimer=0;sidebarTransitionAnimations.splice(0).forEach((animation)=>animation.cancel());sidebarTransitionSnapshot?.remove();sidebarTransitionSnapshot=null;document.documentElement.classList.remove("sidebar-transitioning");}
function sidebarMotionDisabled(){return document.documentElement.dataset.motion==="minimal"||matchMedia("(prefers-reduced-motion: reduce)").matches||typeof Element.prototype.animate!=="function";}
function anonymousSidebarClone(sidebar,rect){const clone=sidebar.cloneNode(true);clone.querySelectorAll("[id]").forEach((node)=>node.removeAttribute("id"));clone.classList.add("sidebar-transition-snapshot");Object.assign(clone.style,{left:`${rect.left}px`,top:`${rect.top}px`,width:`${rect.width}px`,height:`${rect.height}px`});clone.querySelector(".sidebar-resize")?.remove();return clone;}
function toggleSidebar(force, persist = true) {
  const collapsed = force ?? !document.body.classList.contains("sidebar-collapsed"),current=document.body.classList.contains("sidebar-collapsed");
  if(collapsed===current)return;
  const sidebar=$("companySidebar"),moving=[$("conversationStage"),$("termPanel")].filter(Boolean),beforeSidebar=sidebar?.getBoundingClientRect(),beforeMoving=moving.map((node)=>({node,rect:node.getBoundingClientRect()})),animate=$("modalLayer").hidden&&!sidebarMotionDisabled()&&beforeSidebar?.width;
  stopSidebarTransition();const generation=++sidebarTransitionGeneration;
  if(animate){document.documentElement.classList.add("sidebar-transitioning");if(collapsed){sidebarTransitionSnapshot=anonymousSidebarClone(sidebar,beforeSidebar);document.body.append(sidebarTransitionSnapshot);}}
  document.body.classList.toggle("sidebar-collapsed",collapsed);
  const toggle=$("sidebarToggle");if(toggle){toggle.setAttribute("aria-label",collapsed?"Expand sidebar":"Collapse sidebar");toggle.title=`${collapsed?"Expand":"Collapse"} sidebar (Ctrl+\\)`;}
  if (persist) localStorage.setItem("phoenix-sidebar-collapsed",String(collapsed));
  if(!animate){dispatchEvent(new CustomEvent("phoenix:sidebar-transition-end"));return;}
  const duration=190,easing="cubic-bezier(.2,.82,.2,1)",afterSidebar=sidebar.getBoundingClientRect();
  beforeMoving.forEach(({node,rect})=>{const after=node.getBoundingClientRect(),dx=rect.left-after.left;if(Math.abs(dx)>.5)sidebarTransitionAnimations.push(node.animate([{transform:`translate3d(${dx}px,0,0)`},{transform:"translate3d(0,0,0)"}],{duration,easing,fill:"both"}));});
  if(collapsed&&sidebarTransitionSnapshot){const hidden=Math.max(0,beforeSidebar.width-afterSidebar.width);sidebarTransitionAnimations.push(sidebarTransitionSnapshot.animate([{clipPath:"inset(0 0 0 0)",opacity:1},{clipPath:`inset(0 ${hidden}px 0 0)`,opacity:.98}],{duration,easing,fill:"both"}));}
  else{const hidden=Math.max(0,afterSidebar.width-beforeSidebar.width);sidebarTransitionAnimations.push(sidebar.animate([{clipPath:`inset(0 ${hidden}px 0 0)`},{clipPath:"inset(0 0 0 0)"}],{duration,easing,fill:"both"}));}
  const finish=()=>{if(generation!==sidebarTransitionGeneration)return;stopSidebarTransition();document.documentElement.classList.remove("sidebar-transitioning");dispatchEvent(new CustomEvent("phoenix:sidebar-transition-end"));};
  sidebarTransitionTimer=setTimeout(finish,duration+70);Promise.allSettled(sidebarTransitionAnimations.map((animation)=>animation.finished)).then(finish);
}
function notify({ title, body = "", item = null, error = false, duration = 5200, key = "" } = {}) {
  if(key&&document.querySelector(`.toast[data-notification-key="${CSS.escape(String(key))}"]`))return null;
  if(String(key).startsWith("completion:")&&item?.kind&&item?.id){
    const now=Date.now(),conversationKey=itemKey(item.kind,item.id),last=state.recentCompletionItems.get(conversationKey);
    if(Number.isFinite(last)&&now-last<6500)return null;
    state.recentCompletionItems.set(conversationKey,now);
    for(const [candidate,timestamp] of state.recentCompletionItems){if(now-timestamp>120000)state.recentCompletionItems.delete(candidate);}
  }
  const node = document.createElement("div"), profile = item ? profileFor(item) : null;
  if(key)node.dataset.notificationKey=String(key);
  node.className = `toast rich ${error ? "error" : ""}`;
  node.setAttribute("role", error ? "alert" : "status");
  node.innerHTML = `${profile ? `<span class="toast-avatar">${avatarSvg(profile,item.kind)}</span>` : `<span class="toast-mark">${error ? "!" : ICONS.check}</span>`}<span class="toast-copy"><strong>${escapeHtml(title || "Phoenix")}</strong>${body ? `<small>${escapeHtml(String(body).replace(/\s+/g," ").slice(0,220))}</small>` : ""}</span><button type="button" aria-label="Dismiss notification"><svg viewBox="0 0 20 20"><path d="m6 6 8 8M14 6l-8 8"/></svg></button>`;
  const dismiss=node.querySelector("button");dismiss.onclick = (event) => {event.stopPropagation();node.remove();};
  if(item){
    node.tabIndex=0;node.classList.add("actionable");node.setAttribute("aria-label",`${title}. Open conversation.`);
    const open=()=>{node.remove();selectItem(item);};
    node.addEventListener("click",(event)=>{if(!event.target.closest("button"))open();});
    node.addEventListener("keydown",(event)=>{if(event.target===node&&!event.defaultPrevented&&(event.key==="Enter"||event.key===" ")){event.preventDefault();open();}});
  }
  $("toastRegion").prepend(node);
  setTimeout(() => node.remove(), Math.max(1800, duration));
  // The always-on gateway owns native notifications and their sound so work
  // that finishes while the desktop is closed still reaches the user. The UI
  // only renders the in-app toast; emitting here as well made every background
  // event appear twice in GNOME's lock-screen notification list.
  return node;
}
function toast(message,error=false) {
  const raw=String(message?.message||message||""),nativeTimeout=/native command timed out|CURRENT_BACKEND_(?:UNAVAILABLE|DISCONNECTED)|phoenix-isolated-invoke/i.test(raw),authorization=/AUTHORIZATION_REQUIRED|BROWSER_.*APPROVAL_REQUIRED/.test(raw);
  const title=error&&nativeTimeout?"Phoenix couldn’t load this view. Try loading it again.":error&&authorization?"This action needs a decision in Phoenix.":raw;
  const owner=state.selected?itemKey(state.selected.kind,state.selected.id):"company";
  const node=notify({title,error,duration:error?7000:3200,key:error?`error:${owner}:${raw}`:""});
  if(node&&error&&title!==raw){const details=document.createElement("details"),summary=document.createElement("summary"),body=document.createElement("pre");summary.textContent="Technical details";body.textContent=raw;details.append(summary,body);node.querySelector(".toast-copy").append(details);}
  return node;
}

function notifyActivityTransitions(previousView,nextView) {
  if(!previousView||document.documentElement.dataset.notificationsEnabled==="false")return;
  for(const next of nextView.activities||[]){
    const item=itemFromActivity(next),previous=(previousView.activities||[]).find((row)=>sameItem(itemFromActivity(row),item));
    if(!previous||sameItem(item,state.selected))continue;
    const revision=Number(next.transcript_revision)||0,previousRevision=Number(previous.transcript_revision)||0;
    const newerUnread=next.unread&&revision>previousRevision,presentation=ACTIVITY_PRESENTATION[next.status];
    // A dropped activity guard only proves idleness. It also happens after
    // failures and cancellation; only an explicit verified outcome says done.
    const notice=presentation?.notice||(["idle","available"].includes(next.status)&&newerUnread?"has an update":"");
    if(!notice||(!newerUnread&&previous.status===next.status))continue;
    const key=`${itemKey(item.kind,item.id)}:${next.status}:${revision}`;if(state.completionNotifications.has(key))continue;
    state.completionNotifications.add(key);
    const profile=profileFor(item),name=displayName(item,profile)||"Coworker";
    const completion=notice==="finished"||notice==="has an update";
    const body=completion?next.title||"Open the conversation to see the update.":next.activity_label||presentation.label;
    notify({title:`${name} ${notice}`,body:humanActivityText(body),item,error:presentation?.tone==="error",duration:7000,key:`${completion?"completion":"activity"}:${key}`});
  }
  if(state.completionNotifications.size>200)state.completionNotifications=new Set([...state.completionNotifications].slice(-120));
}

function directoryViewChanged(previous,next) {
  return previous?.directory?.as_of_seq!==next?.directory?.as_of_seq
    || JSON.stringify(previous?.activities||[])!==JSON.stringify(next?.activities||[]);
}

async function refreshDirectoryActivity() {
  if (document.hidden || state.dragging || !state.view) return;
  const requestedView = state.view;
  try {
    const view = await directoryCommand({ action:"status" });
    // Do not replace a successful local mutation with an older in-flight poll.
    if (state.view !== requestedView || !directoryViewChanged(state.view,view)) return;
    const previous = state.view;
    notifyActivityTransitions(previous,view);
    state.view = view;
    render();
    dispatchEvent(new CustomEvent("phoenix:directory-status", { detail:{ view } }));
  } catch {}
}

function bindChrome() {
  bindWindowChrome();
  $("sidebarToggle").onclick = () => toggleSidebar(); $("sidebarWake")?.addEventListener("click",() => toggleSidebar(false)); $("filterButton").onclick = openFilter; $("profileButton").onclick = toggleThemeFromButton; syncThemeToggle();
  $("sidebarActions")?.addEventListener("click",(event)=>{
    const action=event.target.closest("[data-company-action]")?.dataset.companyAction;
    if(!action)return;
    if(action==="create-agent")openCreateModal("agent");
    else if(action==="create-group")openCreateModal("group");
    else if(action==="workflows")window.dispatchEvent(new CustomEvent("phoenix:open-settings",{detail:{section:"Workflows & Routines"}}));
    else if(action==="connections")window.dispatchEvent(new CustomEvent("phoenix:open-settings",{detail:{section:"Connections & skills"}}));
    else if(action==="memory")window.dispatchEvent(new CustomEvent("phoenix:open-settings",{detail:{section:"Memory"}}));
  });
  $("settingsButton").onclick = () => window.dispatchEvent(new CustomEvent("phoenix:open-settings"));
  $("stageMore").onclick = (event) => openRowMenu(event.currentTarget,state.selected);
  $("companySearch").addEventListener("input",(event) => { state.query = event.target.value; render(); });
  const railSearch=$("companySearch").closest(".search-box");
  railSearch.tabIndex=0;railSearch.setAttribute("aria-label","Search your company");
  const openRailSearch=()=>{if(document.body.classList.contains("sidebar-collapsed")){toggleSidebar(false);$("companySearch").focus();}};
  railSearch.addEventListener("click",openRailSearch);
  railSearch.addEventListener("keydown",event=>{if(event.target===railSearch&&["Enter"," "].includes(event.key)){event.preventDefault();openRailSearch();$("companySearch").focus();}});
  addEventListener("keydown",(event) => {
    if(document.body.classList.contains("onboarding-open") || event.defaultPrevented)return;
    if ((event.ctrlKey||event.metaKey) && event.key === "\\") { event.preventDefault(); toggleSidebar(); return; }
    handleOverlayKeydown(event);
  });
  const resize = $("sidebarResize"); let startX=0,startW=0,resizing=false;
  const finishSidebarResize=async(event,persist=true)=>{
    if(!resizing)return;
    resizing=false;
    if(event&&resize.hasPointerCapture(event.pointerId))resize.releasePointerCapture(event.pointerId);
    resize.classList.remove("dragging");
    document.documentElement.classList.remove("sidebar-resizing");
    window.getSelection()?.removeAllRanges();
    if(!persist)return;
    // The drag works in real screen px, but the stored width is the unscaled
    // base — otherwise reloading would multiply it by --interface-scale again
    // and the sidebar would creep wider every time.
    const scale=Number(getComputedStyle(document.documentElement).getPropertyValue("--interface-scale"))||1;
    const width=snapSidebarWidth(parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--sidebar-width"))/scale);
    localStorage.setItem("phoenix-sidebar-width",width);
    try{await window.PhoenixSettings?.setGlobalSetting("sidebar.width",Math.round(width));}catch(error){toast(`Could not save sidebar width: ${error.message||error}`,true);}
  };
  resize.addEventListener("pointerdown",(event) => { event.preventDefault();startX=event.clientX;startW=$("companySidebar").getBoundingClientRect().width;resizing=true;window.getSelection()?.removeAllRanges();document.documentElement.classList.add("sidebar-resizing");resize.classList.add("dragging");resize.setPointerCapture(event.pointerId); });
  resize.addEventListener("pointermove",(event) => { if (!resize.hasPointerCapture(event.pointerId)) return; const width=snapSidebarWidth(startW+event.clientX-startX); document.documentElement.style.setProperty("--sidebar-width",`${width}px`); });
  resize.addEventListener("pointerup",(event) => finishSidebarResize(event));
  resize.addEventListener("pointercancel",(event) => finishSidebarResize(event,false));
  resize.addEventListener("lostpointercapture",(event) => finishSidebarResize(event));
}

function announceDirectoryReady(runtimeError="") {
  window.dispatchEvent(new CustomEvent("phoenix:directory-ready", {
    detail: {
      item: state.selected,
      sessionId: activityFor(state.selected)?.canonical_session_id,
      view: state.view,
      runtimeError,
    },
  }));
}

window.PhoenixUI = Object.freeze({
  TAURI,
  SIDEBAR_PREVIEW,
  state,
  invoke,
  wsUrl,
  directoryCommand,
  profileFor,
  activityFor,
  displayName,
  roleTitle,
  profileColor,
  avatarSvg,
  checkIcon: ICONS.check,
  AGENT_PALETTE,
  colorField,
  bindColorField,
  nearestPaletteColor,
  openMenuSelect,
  providerIcon,
  providerLabel,
  escapeHtml,
  wireItem,
  sameItem,
  openPopover,
  closeLayers,
  showModal,
  closeModal,
  toast,
  notify,
  selectItem,
  setTheme,
  resolvedTheme,
  visualPrefs,
  applyVisualPrefs,
  phoenixLogoSource,
  phoenixLogoMarkup,
  configureItem:openEditModal,
  deleteItem:openDeleteModal,
  createItem:(kind)=>openCreateModal(kind==="group"?"group":"agent"),
  refreshDirectory:render,
});

function bindScrollIdle() {
  let idle = 0;
  const root = document.documentElement;
  const mark = () => {
    if (!root.classList.contains("is-scrolling")) root.classList.add("is-scrolling");
    clearTimeout(idle);
    idle = setTimeout(() => root.classList.remove("is-scrolling"), 160);
  };
  document.addEventListener("scroll", mark, { passive: true, capture: true });
  document.addEventListener("wheel", mark, { passive: true, capture: true });
}

async function boot() {
  bindScrollIdle();
  loadPhoenixLogo();
  setTheme(localStorage.getItem("phoenix-theme") || "light");
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    if ((localStorage.getItem("phoenix-theme") || "light") === "system") setTheme("system");
  });
  applyVisualPrefs();
  const width = Number(localStorage.getItem("phoenix-sidebar-width")); if (width) document.documentElement.style.setProperty("--sidebar-width",`calc(${snapSidebarWidth(width)}px * var(--interface-scale))`);
  if (narrowSidebarViewport()) toggleSidebar(true, false);
  else if (localStorage.getItem("phoenix-sidebar-collapsed") === "true") toggleSidebar(true);
  bindChrome();
  let beforeCompact=document.body.classList.contains('sidebar-collapsed');
  matchMedia('(max-width: 560px)').addEventListener('change',event=>{if(event.matches){beforeCompact=document.body.classList.contains('sidebar-collapsed');toggleSidebar(true,false);}else toggleSidebar(beforeCompact,false);});
  if (!TAURI || SIDEBAR_PREVIEW) {
    if (window.PhoenixFluffyFixture) await PhoenixFluffyFixture.ready;
    state.view = window.PhoenixFluffyFixture ? PhoenixFluffyFixture.restore(mockView()) : mockView();
    if(new URLSearchParams(location.search).get("shot")==="group")state.selected={kind:"group",id:"launch-room"};
    render(); announceDirectoryReady();
    if (new URLSearchParams(location.search).get("shot") === "notify") {
      setTimeout(() => notify({ title:"Leo finished", body:"The runtime ownership audit passed. Helper results stayed internal and Leo returned the integrated answer.", item:{kind:"agent",id:"coder"}, duration:60000 }), 250);
    }
    return;
  }
  try {
    await invoke("gateway_ensure");
    const [token,status] = await Promise.all([invoke("gateway_token"),invoke("gateway_status")]);
    state.gateway.token = token; state.gateway.port = status.ws_port || 17373;
    state.view = await directoryCommand({ action:"status" }); restoreSelection();render(); announceDirectoryReady();
    state.refreshTimer = setInterval(refreshDirectoryActivity,4000);
  } catch (error) { const message=error.message||String(error); state.view = emptyView(); render(); announceDirectoryReady(message); toast(`Live company unavailable: ${message}`,true); }
}

boot();
