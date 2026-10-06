#!/usr/bin/env node

import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";

const host = "127.0.0.1";
const port = Number(process.env.PHOENIX_GATEWAY_WS_PORT || "7469");
const sessionId = process.env.PHOENIX_WORKER_AUTH_SESSION || "agent-school_coach";
const targetAgent = process.env.PHOENIX_WORKER_AUTH_AGENT || "school_coach";
const workspace = process.env.PHOENIX_WORKER_AUTH_WORKSPACE || join(homedir(), ".phoenix", "workspace");
const timeoutMs = Number(process.env.PHOENIX_WORKER_AUTH_TIMEOUT_MS || 12 * 60 * 1000);
const token = (await readFile(join(homedir(), ".phoenix", "gateway.token"), "utf8")).trim();
const url = `ws://${host}:${port}/?token=${encodeURIComponent(token)}`;

function requestOnce(request, timeout = 20_000) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url);
    let settled = false;
    const finish = (callback, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      try { socket.close(); } catch {}
      callback(value);
    };
    const timer = setTimeout(() => finish(reject, new Error("gateway request timed out")), timeout);
    socket.addEventListener("open", () => socket.send(JSON.stringify(request)), { once: true });
    socket.addEventListener("message", (event) => {
      try { finish(resolve, JSON.parse(String(event.data))); }
      catch (error) { finish(reject, error); }
    });
    socket.addEventListener("error", () => finish(reject, new Error("gateway socket failed")), { once: true });
  });
}

function conversationAsks(reply) {
  return Array.isArray(reply?.ConversationAsks) ? reply.ConversationAsks : [];
}

function safeStory(event) {
  if (!event || typeof event !== "object") return null;
  const row = { kind: event.kind || "unknown" };
  for (const key of ["agent", "tool", "target", "subject", "worker_id", "label", "item_id", "status", "ok"]) {
    if (event[key] !== undefined) row[key] = event[key];
  }
  const visible = event.text ?? event.markdown ?? event.body ?? event.detail ?? event.output_summary;
  if (typeof visible === "string" && visible.trim()) row.visible = visible.trim().slice(0, 1_200);
  return row;
}

async function runTurn() {
  const turnId = `worker-auth-live-${Date.now()}`;
  const prompt = [
    "This is a read-only live acceptance probe for worker authentication inheritance. Do not do coursework, submit anything, change any external data, or ask the user for login.",
    "Use volume_work exactly once with exactly two jobs and max_concurrency 1, in this order: credential-and-login, then inherited-session-followup. Do not add any other job.",
    "Job credential-and-login must call credential_list and report only whether matching credential metadata for vvs-moodle.pembinahills.ca is visible (never reveal any secret). Then open https://vvs-moodle.pembinahills.ca/my/ and report the auth state.",
    "If the first job is logged out but matching credential metadata is visible, use browser_input_credential to fill the saved credential, refresh browser_state before choosing the submit control, complete the normal sign-in, and verify /my/ again.",
    "Job inherited-session-followup must open https://vvs-moodle.pembinahills.ca/my/ in its own worker tab and report whether it immediately inherited the signed-in session produced by the first job. It must not enter credentials or ask a login question.",
    "If a user-only factor blocks sign-in, report that blocker in the worker result without calling ask_for_login or ask_user.",
    "Return a compact pass/fail report for: inherited vault metadata, inherited browser authentication, and zero login questions.",
  ].join(" ");
  const request = {
    Turn: {
      session_id: sessionId,
      turn_id: turnId,
      user_request: prompt,
      interaction_mode: "execute",
      permission_mode: "full_access",
      yolo: null,
      workspace,
      journal: true,
      target_agent: targetAgent,
      target_group: null,
      group_activation: null,
      delivery: "queue",
      sticky_notes: null,
      viewport: null,
      attachments: null,
    },
  };
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url);
    const stories = [];
    const workers = [];
    let settled = false;
    const finish = (callback, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      try { socket.close(); } catch {}
      callback(value);
    };
    const timer = setTimeout(() => finish(reject, new Error("live worker-auth turn timed out")), timeoutMs);
    socket.addEventListener("open", () => socket.send(JSON.stringify(request)), { once: true });
    socket.addEventListener("message", (event) => {
      let value;
      try { value = JSON.parse(String(event.data)); }
      catch (error) { finish(reject, error); return; }
      if (value?.Story) {
        const row = safeStory(value.Story);
        if (row) stories.push(row);
        if (value.Story.kind === "subagent_lifecycle") workers.push(row);
      }
      if (value?.Error) finish(reject, new Error(value.Error.message || "Phoenix turn failed"));
      if (value?.Done) finish(resolve, { turnId, done: value.Done, stories, workers });
    });
    socket.addEventListener("error", () => finish(reject, new Error("live worker-auth socket failed")), { once: true });
    socket.addEventListener("close", () => {
      if (!settled) finish(reject, new Error("live worker-auth socket closed before Done"));
    }, { once: true });
  });
}

const before = conversationAsks(await requestOnce({ ConversationAsks: { session_id: sessionId, owner: null } }));
const result = await runTurn();
const after = conversationAsks(await requestOnce({ ConversationAsks: { session_id: sessionId, owner: null } }));
const beforeIds = new Set(before.map((ask) => ask.id || ask.ask_id).filter(Boolean));
const newAsks = after.filter((ask) => !beforeIds.has(ask.id || ask.ask_id));
const finalText = String(result.done?.final_markdown || "");
const started = result.workers.filter((event) => String(event.status).toLowerCase() === "started").length;
const completed = result.workers.filter((event) => String(event.status).toLowerCase() === "completed").length;
const vaultPassed = /inherited vault metadata[^]*?\bpass\b/i.test(finalText);
const browserPassed = /inherited browser authentication[^]*?\bpass\b/i.test(finalText);
const report = {
  ok: newAsks.length === 0 && started === 2 && completed === 2 && vaultPassed && browserPassed,
  sessionId,
  targetAgent,
  turnId: result.turnId,
  asksBefore: before.length,
  asksAfter: after.length,
  newAskCount: newAsks.length,
  workerStartedCount: started,
  workerCompletedCount: completed,
  vaultPassed,
  browserPassed,
  final: finalText.slice(0, 4_000),
  stories: result.stories.slice(-80),
};

console.log(`PHOENIX_WORKER_AUTH_LIVE ${JSON.stringify(report)}`);
if (!report.ok) process.exitCode = 2;
