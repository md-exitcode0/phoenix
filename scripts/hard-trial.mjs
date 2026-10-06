#!/usr/bin/env node
// Generic isolated hard-trial runner.
//
//   node scripts/hard-trial.mjs <binary> <new-output-dir> <role> <prompt-file> [--seed DIR] [--minutes N]
//
// Starts a private gateway (scripts/lib/acceptance-gateway.mjs), submits one
// Execute/Full-Access turn to the company agent with internal role <role>,
// records every frame, then writes summary.json: elapsed, rounds, tokens,
// cache ratio, tool counts, and repeated identical tool calls — the numbers
// that show whether a change made Phoenix cheaper and less loopy.
import assert from 'node:assert/strict';
import {cp, mkdir, open, readFile, writeFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {messages, once, withAcceptanceGateway} from './lib/acceptance-gateway.mjs';

const args = process.argv.slice(2);
const flag = name => { const i = args.indexOf(name); if (i < 0) return null; const [, v] = args.splice(i, 2); return v; };
const seed = flag('--seed');
const minutes = Number(flag('--minutes') ?? 45);
const [binaryArg, outputArg, role, promptFile] = args;
assert.ok(binaryArg?.startsWith('/') && outputArg?.startsWith('/') && role && promptFile, 'usage: <binary> <output> <role> <prompt-file>');
const output = resolve(outputArg);
const workspace = join(output, 'work');
const prompt = (await readFile(promptFile, 'utf8')).trim();
await mkdir(output, {mode: 0o700});
if (seed) await cp(resolve(seed), workspace, {recursive: true, errorOnExist: true});
else await mkdir(workspace);
await writeFile(join(output, 'prompt.txt'), prompt + '\n');

await withAcceptanceGateway({binary: resolve(binaryArg), output, workspace, accountPool: true, memory: false,
  nativeVision: true, preserveState: true}, async ({socketPath}) => {
  await once(socketPath, {Onboarding: {action: 'choose_company', choice: 'founding_company'}});
  const reply = await once(socketPath, {CompanyDirectory: {action: 'status'}});
  const agents = reply.CompanyDirectory?.directory?.agents ?? [];
  const actor = agents.find(a => (a.profile ?? a).internal_role === role || (a.profile ?? a).agent_id === role);
  assert.ok(actor, `no company agent with role ${role}`);
  const profile = actor.profile ?? actor;
  const sessionId = profile.canonical_session_id, targetAgent = profile.agent_id;
  const submission = {Turn: {session_id: sessionId, turn_id: `trial-${Date.now()}`, user_request: prompt, workspace,
    permission_mode: 'full_access', interaction_mode: 'execute', journal: true, delivery: 'queue', target_agent: targetAgent}};
  await writeFile(join(output, 'submission.json'), JSON.stringify(submission, null, 2) + '\n');
  const log = await open(join(output, 'events.jsonl'), 'wx', 0o600);
  const started = Date.now();
  let terminal = null, failure = null;
  const timer = setTimeout(() => once(socketPath, {Cancel: {session_id: sessionId, target_agent: targetAgent}}, 10_000).catch(() => {}), minutes * 60_000);
  try {
    for await (const frame of messages(socketPath, submission, {signal: AbortSignal.timeout(minutes * 60_000 + 60_000)})) {
      await log.write(JSON.stringify({elapsed_ms: Date.now() - started, frame}) + '\n');
      const s = frame.Story;
      if (s && ['tool', 'handoff', 'answer', 'warning'].includes(s.kind))
        console.log(JSON.stringify({t: Math.round((Date.now() - started) / 1000), kind: s.kind, agent: s.agent, tool: s.tool, ok: s.ok,
          target: typeof s.target === 'string' ? s.target.slice(0, 120) : undefined}));
      if (frame.Done || frame.Error) { terminal = frame; break; }
    }
  } catch (error) { failure = String(error?.stack ?? error); }
  finally { clearTimeout(timer); await log.close(); }
  await writeFile(join(output, 'terminal.json'), JSON.stringify({elapsed_ms: Date.now() - started, terminal, failure}, null, 2) + '\n');
});

// Summary from the preserved round timings and the event journal.
const rounds = (await readFile(join(output, 'durable-state/runs/round_timings.jsonl'), 'utf8').catch(() => ''))
  .split('\n').filter(Boolean).map(line => JSON.parse(line));
const events = (await readFile(join(output, 'events.jsonl'), 'utf8')).split('\n').filter(Boolean).map(line => JSON.parse(line));
const tools = events.map(e => e.frame.Story).filter(s => s?.kind === 'tool');
const seen = new Map();
for (const t of tools) { const key = `${t.agent}|${t.tool}|${t.target}`; seen.set(key, (seen.get(key) ?? 0) + 1); }
const byAgent = {};
for (const r of rounds) {
  const a = byAgent[r.agent] ??= {rounds: 0, input: 0, cached: 0, output: 0, peak: 0};
  a.rounds++; a.input += r.input_tokens || 0; a.cached += r.cache_read_tokens || 0; a.output += r.output_tokens || 0;
  a.peak = Math.max(a.peak, r.input_tokens || 0);
}
const total = Object.values(byAgent).reduce((s, a) => ({input: s.input + a.input, cached: s.cached + a.cached, output: s.output + a.output}), {input: 0, cached: 0, output: 0});
const terminal = JSON.parse(await readFile(join(output, 'terminal.json'), 'utf8'));
const summary = {
  elapsed_min: +(terminal.elapsed_ms / 60000).toFixed(1),
  completion: terminal.terminal?.Done?.completion ?? (terminal.terminal?.Error ? 'error' : 'none'),
  rounds: rounds.length, ...total, cache_ratio: +(total.cached / Math.max(1, total.input)).toFixed(3),
  uncached_input: total.input - total.cached, byAgent,
  tool_calls: tools.length,
  tool_counts: Object.fromEntries(Object.entries(tools.reduce((m, t) => (m[t.tool] = (m[t.tool] ?? 0) + 1, m), {})).sort((a, b) => b[1] - a[1])),
  repeated_identical_calls: [...seen].filter(([, n]) => n > 1).map(([k, n]) => ({call: k.slice(0, 200), n})),
  handoffs: events.map(e => e.frame.Story).filter(s => s?.kind === 'handoff').length,
};
await writeFile(join(output, 'summary.json'), JSON.stringify(summary, null, 2) + '\n');
console.log(JSON.stringify({phase: 'summary', ...summary, byAgent: undefined, repeated_identical_calls: summary.repeated_identical_calls.length}));
