import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const context = vm.createContext({});
const source = readFileSync(new URL('../js/projects.js', import.meta.url), 'utf8');
vm.runInContext(source.replace(/^import .*;\s*$/gm, '').replace(/export function /g, 'function '), context);
const group = entries => JSON.parse(JSON.stringify(context.groupChatHistory(entries)));

test('only consecutive tools form batches; single calls, reasoning and skills remain separate', () => {
  const user = { role: 'user', text: 'Task' };
  const read = { role: 'tool', tool: { id: 'a' } };
  const think = { role: 'assistant', thinking: 'plan', text: '' };
  const skill = { role: 'notice', text: 'Использован скилл: test' };
  const edit = { role: 'tool', tool: { id: 'b' } };
  const check = { role: 'tool', tool: { id: 'c' } };
  const answer = { role: 'assistant', text: 'Changed it' };
  assert.deepEqual(group([user, read, think, skill, edit, check, answer]), [
    { kind: 'message', message: user },
    { kind: 'tool', message: read },
    { kind: 'message', message: think },
    { kind: 'message', message: skill },
    { kind: 'activity', entries: [edit, check] },
    { kind: 'message', message: answer },
  ]);
  assert.deepEqual(group([read]), [{ kind: 'tool', message: read }]);
  assert.deepEqual(group([read, edit]), [{ kind: 'activity', entries: [read, edit] }]);
  assert.deepEqual(group([]), []);
});

test('activity auto-opens during work and closes afterward unless manually overridden', () => {
  const open = context.activityOpen;
  assert.equal(open(true, undefined), true);
  assert.equal(open(false, undefined), false);
  assert.equal(open(true, false), false);
  assert.equal(open(false, true), true);
});
