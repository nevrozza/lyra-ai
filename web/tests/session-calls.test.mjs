import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const context = vm.createContext({ duration: ms => Math.floor(ms / 60_000) + ':00' });
vm.runInContext(readFileSync(new URL('../js/sessions.js', import.meta.url), 'utf8').replace(/^import .*;\s*$/gm, '').replace(/export function /g, 'function '), context);
test('interleaved events stay associated with their actual call', () => {
  const events = [{ call: 1, id: 1 }, { call: 2, id: 2 }, { call: 1, id: 3 }];
  const grouped = context.groupCallEvents(events);
  assert.deepEqual(Array.from(grouped.get(1), event => event.id), [1, 3]);
  assert.deepEqual(Array.from(grouped.get(2), event => event.id), [2]);
});
test('short call duration remains readable', () => {
  assert.equal(context.formatCallDuration(8), '8 мс');
  assert.equal(context.formatCallDuration(2500), '2.5 с');
  assert.equal(context.formatCallDuration(null), '');
});
