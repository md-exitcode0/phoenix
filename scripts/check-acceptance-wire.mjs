import assert from 'node:assert/strict';
import net from 'node:net';
import {mkdtemp, rm} from 'node:fs/promises';
import {join} from 'node:path';
import {tmpdir} from 'node:os';
import {once, messages} from './lib/acceptance-gateway.mjs';

const root = await mkdtemp(join(tmpdir(), 'phoenix-wire-check-'));
const socketPath = join(root, 'gateway.sock');
const sockets = new Set();
const server = net.createServer(socket => {
  sockets.add(socket); socket.once('close', () => sockets.delete(socket));
  socket.once('data', request => {
    const kind = JSON.parse(request.toString());
    if (kind === 'unicode') {
      const bytes = Buffer.from(JSON.stringify({text:'Café Ελληνικά 日本語 😀'}) + '\n');
      let index = 0;
      const send = () => {
        if (index < bytes.length) {socket.write(bytes.subarray(index, ++index)); setImmediate(send);}
        else socket.end();
      };
      send();
    } else if (kind === 'truncated') socket.end('{"Done":');
    else if (kind === 'hang') { /* timeout must close the observer socket */ }
    else socket.end('"Pong"\n');
  });
});
await new Promise((resolve, reject) => {server.once('error', reject); server.listen(socketPath, resolve);});
try {
  assert.equal(await once(socketPath, 'Ping'), 'Pong');
  assert.deepEqual(await once(socketPath, 'unicode'), {text:'Café Ελληνικά 日本語 😀'});
  await assert.rejects(async () => {for await (const value of messages(socketPath, 'truncated')) void value;}, /incomplete event/);
  await assert.rejects(once(socketPath, 'hang', 30));
  console.log('ACCEPTANCE_WIRE_OK: Unicode split across bytes, partial-event rejection, bounded observation');
} finally {
  for (const socket of sockets) socket.destroy();
  await new Promise(resolve => server.close(resolve));
  await rm(root, {recursive:true});
}
