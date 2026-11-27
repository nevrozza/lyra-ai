import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const source = readFileSync(new URL('../js/commands.js', import.meta.url), 'utf8');
const context = vm.createContext({});
vm.runInContext(source.replace(/^import .*;\s*$/gm, '').replace(/export function /g, 'function '), context);
const locate = context.commandLocation;

test('command approval is visible only in its owning project chat', () => {
  const command = { id: 1, owner: { project_id: 4, chat_id: 8 } };
  assert.equal(locate(command, { projectId: 4, chatId: 8 }), 'chat');
  assert.equal(locate(command, { projectId: 4, chatId: 9 }), 'other');
  assert.equal(locate(command, { projectId: 5, chatId: 8 }), 'other');
  assert.equal(locate(command, { projectId: 4 }), 'other');
  assert.equal(locate(command, undefined), 'other');
  assert.equal(locate({ id: 2, owner: null }, { projectId: 4, chatId: 8 }), 'external');
  assert.equal(locate(null, undefined), null);
});
