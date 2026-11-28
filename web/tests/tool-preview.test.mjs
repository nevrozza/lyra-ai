import assert from 'node:assert/strict';
import test from 'node:test';
import vm from 'node:vm';
import { readFileSync } from 'node:fs';
const context = vm.createContext({});
vm.runInContext(readFileSync(new URL('../js/tool-preview.js', import.meta.url), 'utf8').replace(/^import .*;\s*$/gm, '').replace(/export function /g, 'function '), context);
test('write preview decodes incomplete JSON without requiring a closed tool call', () => {
  const raw = JSON.stringify({path:'src/ui.js',content:'let x = "hello";\n// мир'}).slice(0,-2);
  const preview = context.toolPreview({name:'write',preview:raw});
  assert.equal(preview.target, 'src/ui.js');
  assert.equal(preview.text, 'let x = "hello";\n// мир');
});
test('partial string tolerates a cut inside an escape', () => {
  const raw = '{"content":"hello' + String.fromCharCode(92) + 'u00';
  assert.equal(context.partialString(raw,'content'), 'hello');
});
test('paths inside escaped content are not mistaken for a path field', () => {
  const raw = JSON.stringify({content:'example: {"path":"wrong"}',path:'right'});
  assert.equal(context.partialString(raw,'path'), 'right');
});
