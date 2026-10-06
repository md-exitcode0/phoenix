"use strict";
// Drop-in for the one adm-zip call electron-chrome-web-store makes:
// `new AdmZip(buffer).extractAllTo(dest, overwrite)`. adm-zip follows symlink
// entries (arbitrary file overwrite, GHSA-vwc7-r8mq-g2x9) and trusts declared
// sizes (GHSA-7q85-xj36-vmfc), with no fixed release. Extensions come from
// third parties, so this extractor never writes symlinks, never leaves the
// destination, and caps what it will inflate.
const fs = require("node:fs");
const path = require("node:path");
const zlib = require("node:zlib");

const MAX_TOTAL_BYTES = 512 * 1024 * 1024;
const MAX_ENTRIES = 20000;

class SafeZip {
  constructor(buffer) {
    if (!Buffer.isBuffer(buffer)) throw new Error("zip input must be a buffer");
    this.buffer = buffer;
  }

  entries() {
    const buf = this.buffer;
    // End of central directory: signature 0x06054b50 within the last 64KB.
    let eocd = -1;
    for (let i = buf.length - 22; i >= Math.max(0, buf.length - 65557); i--) {
      if (buf.readUInt32LE(i) === 0x06054b50) { eocd = i; break; }
    }
    if (eocd < 0) throw new Error("not a zip archive");
    const count = buf.readUInt16LE(eocd + 10);
    let offset = buf.readUInt32LE(eocd + 16);
    if (count > MAX_ENTRIES) throw new Error("zip has too many entries");
    const out = [];
    for (let n = 0; n < count; n++) {
      if (buf.readUInt32LE(offset) !== 0x02014b50) throw new Error("corrupt zip central directory");
      const method = buf.readUInt16LE(offset + 10);
      const compressed = buf.readUInt32LE(offset + 20);
      const size = buf.readUInt32LE(offset + 24);
      const nameLength = buf.readUInt16LE(offset + 28);
      const extraLength = buf.readUInt16LE(offset + 30);
      const commentLength = buf.readUInt16LE(offset + 32);
      const externalAttributes = buf.readUInt32LE(offset + 38);
      const localOffset = buf.readUInt32LE(offset + 42);
      const name = buf.toString("utf8", offset + 46, offset + 46 + nameLength);
      out.push({ name, method, compressed, size, localOffset, mode: (externalAttributes >>> 16) & 0xffff });
      offset += 46 + nameLength + extraLength + commentLength;
    }
    return out;
  }

  extractAllTo(destination, _overwrite = true) {
    const root = path.resolve(destination);
    fs.mkdirSync(root, { recursive: true });
    let total = 0;
    for (const entry of this.entries()) {
      const name = entry.name.replace(/\\/g, "/");
      if (!name || name.startsWith("/") || /^[a-z]:/i.test(name) || name.split("/").includes("..")) {
        throw new Error(`zip entry escapes the destination: ${entry.name}`);
      }
      const isLink = (entry.mode & 0o170000) === 0o120000;
      if (isLink) continue; // never materialize symlinks from an archive
      const target = path.resolve(root, name);
      if (target !== root && !target.startsWith(root + path.sep)) throw new Error(`zip entry escapes the destination: ${entry.name}`);
      if (name.endsWith("/")) { fs.mkdirSync(target, { recursive: true }); continue; }
      total += entry.size;
      if (total > MAX_TOTAL_BYTES) throw new Error("zip expands beyond the size limit");
      const local = entry.localOffset;
      if (this.buffer.readUInt32LE(local) !== 0x04034b50) throw new Error("corrupt zip local header");
      const start = local + 30 + this.buffer.readUInt16LE(local + 26) + this.buffer.readUInt16LE(local + 28);
      const data = this.buffer.subarray(start, start + entry.compressed);
      let content;
      if (entry.method === 0) content = data;
      else if (entry.method === 8) content = zlib.inflateRawSync(data, { maxOutputLength: Math.max(entry.size, 1) });
      else throw new Error(`unsupported zip compression method ${entry.method}`);
      if (content.length !== entry.size) throw new Error(`zip entry size mismatch: ${entry.name}`);
      fs.mkdirSync(path.dirname(target), { recursive: true });
      // Refuse to write through an existing symlink at the target path.
      try { if (fs.lstatSync(target).isSymbolicLink()) throw new Error(`refusing to overwrite a symlink: ${entry.name}`); } catch (error) { if (error.code !== "ENOENT") throw error; }
      fs.writeFileSync(target, content);
    }
  }
}

module.exports = SafeZip;
module.exports.default = SafeZip;
