import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const context = vm.createContext({});
const source = readFileSync(new URL('../js/projects.js', import.meta.url), 'utf8');
vm.runInContext(source.replace(/^import .*;\s*$/gm, '').replace(/export function /g, 'function '), context);

test('Enter sends, Shift+Enter and IME composition remain native', () => {
  const submit = context.shouldSubmitPrompt;
  assert.equal(submit({ key: 'Enter' }), true);
  for (const modifier of ['shiftKey', 'isComposing', 'ctrlKey', 'altKey', 'metaKey']) {
    assert.equal(submit({ key: 'Enter', [modifier]: true }), false);
  }
  assert.equal(submit({ key: 'Enter', keyCode: 229 }), false);
  assert.equal(submit({ key: 'a' }), false);
});
