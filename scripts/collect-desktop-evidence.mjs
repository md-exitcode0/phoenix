// Copy only screenshot evidence in this run's private temporary root. Never
// substitute these observer files for the agent's requested preview artifact.
import {readdir, lstat, mkdir, copyFile, writeFile, readFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {createHash} from 'node:crypto';

export async function collectDesktopEvidence(home, destination) {
  if (!home.startsWith('/tmp/phoenix-ui-acceptance-')) throw Error('Not an acceptance home');
  const root = join(home, 'tmp');
  await mkdir(destination, {recursive:true, mode:0o700});
  const paths = [];
  async function visit(dir, depth = 0) {
    if (depth > 3) return;
    for (const entry of await readdir(dir, {withFileTypes:true})) {
      const path = join(dir, entry.name);
      if (entry.isSymbolicLink()) continue;
      if (entry.isDirectory() && (depth > 0 || entry.name.startsWith('phoenix-desktop-shots-')))
        await visit(path, depth + 1);
      else if (depth > 0 && entry.isFile() && /\.png$/.test(entry.name)) paths.push(path);
    }
  }
  try {await visit(root);} catch (error) {if (error.code !== 'ENOENT') throw error;}
  const manifest = [];
  for (const path of paths) {
    const meta = await lstat(path);
    if (!meta.isFile() || meta.size > 64*1024*1024) continue;
    const bytes = await readFile(path);
    if (!bytes.subarray(0,8).equals(Buffer.from([137,80,78,71,13,10,26,10]))) continue;
    const name = path.slice(root.length+1).replaceAll('/', '--');
    await copyFile(path, join(destination, name));
    manifest.push({source:path, file:name, modified_ms:meta.mtimeMs,
      sha256:createHash('sha256').update(bytes).digest('hex'), bytes:bytes.length});
  }
  manifest.sort((a,b)=>a.modified_ms-b.modified_ms);
  await writeFile(join(destination,'manifest.json'), JSON.stringify(manifest,null,2));
  return manifest;
}

if (process.argv[1] && resolve(process.argv[1]) === new URL(import.meta.url).pathname) {
  const [home,destination] = process.argv.slice(2);
  console.log(JSON.stringify(await collectDesktopEvidence(home,destination)));
}
