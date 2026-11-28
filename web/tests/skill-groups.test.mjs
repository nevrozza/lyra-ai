import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const context = vm.createContext({});
vm.runInContext(readFileSync(new URL('../js/skills.js', import.meta.url), 'utf8').replace(/^import .*;\s*$/gm, '').replace(/export function /g, 'function '), context);
test('custom, built-in and role skills are mutually exclusive, with roles taking precedence', () => {
  const skills = [{ name:'role', builtin:true, role_template:true }, { name:'custom', builtin:false }, { name:'builtin', builtin:true }];
  const groups = context.groupSkills(skills);
  assert.deepEqual(Array.from(groups.custom, skill => skill.name), ['custom']);
  assert.deepEqual(Array.from(groups.builtin, skill => skill.name), ['builtin']);
  assert.deepEqual(Array.from(groups.roles, skill => skill.name), ['role']);
});
