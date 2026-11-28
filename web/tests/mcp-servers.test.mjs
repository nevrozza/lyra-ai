import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const context = vm.createContext({ URL });
vm.runInContext(readFileSync(new URL('../js/mcp-servers.js', import.meta.url), 'utf8').replace(/^import .*;\s*$/gm, '').replace(/export function /g, 'function '), context);
const plain = value => JSON.parse(JSON.stringify(value));

test('form preserves supported local fields and removes remote-only settings', () => {
  const fields = { transport: 'stdio', enabled: false, description: 'Files', exposure: 'hidden', timeout: '45',
    args: ['', 'a b'], command: 'npx', cwd: 'C:/folder', env: { TOKEN: '${TOKEN}' },
    tools: { write: 'hidden' }, url: 'https://example.com/mcp', headers: { Authorization: 'secret' } };
  assert.deepEqual(plain(context.buildServer(fields)), { type: 'stdio', enabled: false, exposure: 'hidden', description: 'Files', timeout: 45, toolExposure: { write: 'hidden' }, command: 'npx', args: ['', 'a b'], cwd: 'C:/folder', env: { TOKEN: '${TOKEN}' } });
  assert.deepEqual(plain(context.buildServer({ ...fields, transport: 'streamable-http', httpType: 'http' })), { type: 'http', enabled: false, exposure: 'hidden', description: 'Files', timeout: 45, toolExposure: { write: 'hidden' }, url: 'https://example.com/mcp', headers: { Authorization: 'secret' } });
});

test('rename and save preserve other servers and reject normalized collisions', () => {
  const config = { mcpServers: { first: { command: 'node' }, 'a-b': { command: 'uvx' } } };
  assert.deepEqual(plain(context.replaceServer(config, 'first', 'new', { command: 'npx' })), { mcpServers: { 'a-b': { command: 'uvx' }, new: { command: 'npx' } } });
  assert.throws(() => context.replaceServer(config, 'first', 'A_B', {}), /уже существует/);
  assert.throws(() => context.replaceServer(config, null, 'bad name', {}));
  assert.equal(config.mcpServers.first.command, 'node');
});
