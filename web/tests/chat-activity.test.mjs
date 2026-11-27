import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const context = vm.createContext({});
const source = readFileSync(new URL('../js/projects.js', import.meta.url), 'utf8');
vm.runInContext(source.replace(/^import .*;\s*$/gm, '').replace(/export function /g, 'function '), context);
const group = entries => JSON.parse(JSON.stringify(context.groupChatHistory(entries)));

test('tool batches group across reasoning and skill notices, not visible messages', () => {
  const user = { role: 'user', text: 'Task' };
  const read = { role: 'tool', tool: { id: 'a' } };
  const think = { role: 'assistant', thinking: 'plan', text: '' };
  const skill = { role: 'notice', text: 'Использован скилл: test' };
  const edit = { role: 'tool', tool: { id: 'b' } };
  const answer = { role: 'assistant', text: 'Changed it' };
  const check = { role: 'tool', tool: { id: 'c' } };
  assert.deepEqual(group([user, read, think, skill, edit, answer, check]), [
    { kind: 'message', message: user },
    { kind: 'activity', entries: [read, think, skill, edit] },
    { kind: 'message', message: answer },
    { kind: 'activity', entries: [check] },
  ]);
});

test('activity auto-opens during work and closes afterward unless manually overridden', () => {
  const open = context.activityOpen;
  assert.equal(open(true, undefined), true);
  assert.equal(open(false, undefined), false);
  assert.equal(open(true, false), false);
  assert.equal(open(false, true), true);
});
