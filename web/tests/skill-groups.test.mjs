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


test('built-in and role groups share the same disclosure header and default states', () => {
  const html = readFileSync(new URL('../index.html', import.meta.url), 'utf8');
  const builtin = html.match(/<details id="builtin-skills"[^>]*>/)?.[0];
  const roles = html.match(/<details id="role-skills"[^>]*>/)?.[0];
  assert.match(builtin, /class="skill-group skill-group-disclosure"/);
  assert.match(roles, /class="skill-group skill-group-disclosure"/);
  assert.match(builtin, /\sopen(?:\s|>)/);
  assert.doesNotMatch(roles, /\sopen(?:\s|>)/);
  for (const id of ['builtin-skills-title', 'role-skills-title']) {
    assert.ok(html.includes('<summary class="skill-group-heading"><h3 id="' + id + '"'));
  }
});
