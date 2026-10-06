#!/usr/bin/env node

import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";

const token = (await readFile(join(homedir(), ".phoenix", "gateway.token"), "utf8")).trim();
if (token.length < 24) throw new Error("Phoenix gateway token is missing");
const port = Number(process.env.PHOENIX_GATEWAY_WS_PORT || "7469");
const socket = new WebSocket(`ws://127.0.0.1:${port}/?token=${encodeURIComponent(token)}`);
const timeout = setTimeout(() => {
  console.error("PHOENIX_GATEWAY_BROWSER_TIMEOUT");
  socket.close();
  process.exitCode = 2;
}, 25000);
socket.addEventListener("open", () => {
  socket.send(JSON.stringify({ BrowserSurface: { instance: "agent-phoenix", action: "open" } }));
});
socket.addEventListener("message", (event) => {
  clearTimeout(timeout);
  const response = JSON.parse(String(event.data));
  console.log(`PHOENIX_GATEWAY_BROWSER ${JSON.stringify(response)}`);
  socket.close();
  if (response.Error) process.exitCode = 2;
});
socket.addEventListener("error", () => {
  clearTimeout(timeout);
  console.error("PHOENIX_GATEWAY_BROWSER_SOCKET_ERROR");
  process.exitCode = 2;
});
