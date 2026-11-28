import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const context = vm.createContext({});
const source = readFileSync(new URL('../js/ui.js', import.meta.url), 'utf8');
vm.runInContext(source.replace(/export /g, ''), context);

test('dialog padding and nested controls are not backdrop clicks', () => {
  const dialog = { getBoundingClientRect: () => ({ left: 10, right: 100, top: 20, bottom: 90 }) };
  const outside = (x, y, target = dialog) => context.isOutsideDialog({ target, clientX: x, clientY: y }, dialog);
  assert.equal(outside(9, 50), true);
  assert.equal(outside(101, 50), true);
  assert.equal(outside(50, 19), true);
  assert.equal(outside(50, 91), true);
  assert.equal(outside(10, 20), false);
  assert.equal(outside(50, 50), false);
  assert.equal(outside(0, 0, {}), false);
});

test('all application dialogs and popovers opt into native light dismissal', () => {
  const html = readFileSync(new URL('../index.html', import.meta.url), 'utf8');
  const dialogs = html.match(/<dialog\b[^>]*>/g);
  assert.ok(dialogs.length > 0);
  assert.ok(dialogs.every(tag => /closedby="any"/.test(tag)));
  const popovers = [...html.matchAll(/\bpopover="([^"]*)"/g)];
  assert.ok(popovers.length > 0);
  assert.ok(popovers.every(match => match[1] === 'auto'));
});


test('static popover triggers are native invokers so a click on the trigger is not outside dismissal', () => {
  const html = readFileSync(new URL('../index.html', import.meta.url), 'utf8');
  for (const id of ['project-workspace-popover', 'chat-model-popover', 'chat-thinking-popover', 'command-settings-popover']) {
    const trigger = [...html.matchAll(/<button\b[^>]*>/g)].map(match => match[0])
      .find(tag => tag.includes('aria-controls="' + id + '"'));
    assert.ok(trigger?.includes('popovertarget="' + id + '"'), id);
  }
});
