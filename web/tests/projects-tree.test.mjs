import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const context = vm.createContext({});
const source = readFileSync(new URL('../js/projects.js', import.meta.url), 'utf8');
vm.runInContext(source.replace(/^import .*;\s*$/gm, '').replace(/export function /g, 'function '), context);

test('new top-level chats and groups appear first, steps stay in ascending order', () => {
  const chats = [
    { id: 2 }, { id: 10, group: { id: 3 } }, { id: 4, group: { id: 3 } },
    { id: 8 }, { id: 7, group: { id: 6 } }, { id: 12, group: { id: 6 } },
  ];
  assert.deepEqual(chats.sort(context.compareTreeChats).map(chat => chat.id), [8, 7, 12, 4, 10, 2]);
});
