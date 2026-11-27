import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const context = vm.createContext({});
const source = readFileSync(new URL('../js/projects.js', import.meta.url), 'utf8');
vm.runInContext(source.replace(/^import .*;\s*$/gm, '').replace(/export function /g, 'function '), context);
const group = entries => JSON.parse(JSON.stringify(context.groupChatHistory(entries)));

test('group intermediate reasoning with tools, but keep skill notices and visible messages separate', () => {
  const user = { role: 'user', text: 'Task' };
  const read = { role: 'tool', tool: { id: 'a' } };
  const think = { role: 'assistant', thinking: 'plan', text: '' };
  const skill = { role: 'notice', text: 'Использован скилл: test' };
  const edit = { role: 'tool', tool: { id: 'b' } };
  const check = { role: 'tool', tool: { id: 'c' } };
  const answer = { role: 'assistant', text: 'Changed it', thinking: 'done' };
  assert.deepEqual(group([user, read, think, skill, edit, check, answer]), [
    { kind: 'message', message: user },
    { kind: 'activity', entries: [read, think] },
    { kind: 'message', message: skill },
    { kind: 'activity', entries: [edit, check] },
    { kind: 'message', message: answer },
  ]);
  assert.deepEqual(group([think, read, think, edit]), [
    { kind: 'activity', entries: [think, read, think, edit] },
  ]);
  assert.deepEqual(group([read]), [{ kind: 'tool', message: read }]);
  assert.deepEqual(group([think]), [{ kind: 'message', message: think }]);
  assert.deepEqual(group([read, skill, edit]), [
    { kind: 'tool', message: read }, { kind: 'message', message: skill }, { kind: 'tool', message: edit },
  ]);
  assert.deepEqual(group([]), []);
});

test('activity auto-opens during work and closes afterward unless manually overridden', () => {
  const open = context.activityOpen;
  assert.equal(open(true, undefined), true);
  assert.equal(open(false, undefined), false);
  assert.equal(open(true, false), false);
  assert.equal(open(false, true), true);
});
