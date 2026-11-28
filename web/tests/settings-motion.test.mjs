import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const context = vm.createContext({});
vm.runInContext(readFileSync(new URL('../js/ui.js', import.meta.url), 'utf8').replace(/export /g, ''), context);

test('settings updates retain the switch node and replace its confirmed state and callback', () => {
  const list = {
    dataset: {}, children: [],
    insertBefore(row, before) {
      const previous = this.children.indexOf(row);
      if (previous >= 0) this.children.splice(previous, 1);
      this.children.splice(before === null ? this.children.length : this.children.indexOf(before), 0, row);
    },
  };
  function row(name, enabled) {
    const attrs = new Map([['aria-checked', String(enabled)]]);
    const control = { getAttribute: key => attrs.get(key) ?? null,
      setAttribute: (key, value) => attrs.set(key, value),
      removeAttribute: key => attrs.delete(key), onclick: () => enabled };
    return { dataset: { setting: name, enabled: String(enabled) },
      firstElementChild: { isEqualNode: () => true }, lastElementChild: { children: [control] },
      classList: { add() {} }, remove() { list.children.splice(list.children.indexOf(this), 1); } };
  }
  const original = row('first', false);
  const second = row('second', true);
  context.reconcileSettingsRows(list, [original, second]);
  const control = original.lastElementChild.children[0];
  context.reconcileSettingsRows(list, [row('first', true), row('second', true)]);
  assert.equal(list.children[0], original);
  assert.equal(control.getAttribute('aria-checked'), 'true');
  assert.equal(control.onclick(), true);
  context.reconcileSettingsRows(list, [row('second', false)]);
  assert.deepEqual(list.children, [second]);
});
