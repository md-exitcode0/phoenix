import assert from 'node:assert/strict';
import {readFileSync, existsSync} from 'node:fs';
import vm from 'node:vm';

const source = readFileSync(new URL('./conversation.js', import.meta.url), 'utf8');
const helper = source.slice(source.indexOf('  function localMarkdownPath('), source.indexOf('  function inlineMarkdown('));
const context = {};
vm.runInNewContext(`${helper}\nthis.decodePath = localMarkdownPath;`, context);
assert.equal(context.decodePath('/workspace/Activity%20Hour%20Plan.pdf'), '/workspace/Activity Hour Plan.pdf');
assert.equal(context.decodePath('/workspace/caf%C3%A9.pdf'), '/workspace/café.pdf');
assert.equal(context.decodePath('/workspace/100% complete.pdf'), '/workspace/100% complete.pdf');
assert.equal(context.decodePath('/workspace/100%2520.pdf'), '/workspace/100%20.pdf');
assert.ok(source.includes('const path=localMarkdownPath(localLink.dataset.openLocalPath||"")'));
if (process.argv[2]) assert.ok(existsSync(context.decodePath(process.argv[2])), 'reported artifact exists at the decoded path');
console.log('PASS: encoded local links, Unicode, literal/malformed percent, and single decoding');
