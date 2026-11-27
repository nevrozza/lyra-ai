import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const source = readFileSync(new URL('../js/telemetry.js', import.meta.url), 'utf8');
const context = vm.createContext({});
vm.runInContext(source.replace(/^import .*;\s*$/gm, '').replace(/export function /g, 'function '), context);
const state = options => context.metricsFreshness({ now: 50_000, ...options })[0];

test('metrics freshness tracks the last successful sample and connection failures', () => {
  assert.equal(state({}), 'waiting');
  assert.equal(state({ sampledAt: 20_001 }), 'fresh');
  assert.equal(state({ sampledAt: 20_000 }), 'stale');
  assert.equal(state({ sampledAt: 49_000, failed: true }), 'offline');
  assert.equal(state({ sampledAt: 49_000, failed: true, closed: true }), 'stopped');
});
