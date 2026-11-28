// Isolated Streamable HTTP MCP discovery. No tool calls, no redirects and no
// credentials in stdout/errors. The Rust parent enforces the overall deadline.
import { readFileSync } from 'node:fs';
const MAX_BYTES = 1024 * 1024;
const MAX_TOOLS = 500;
let session, version = '2025-03-26', url, configuredHeaders;
const versions = new Set(['2024-11-05','2025-03-26','2025-06-18','2025-11-25']);

async function readResult(response, id) {
  if (!response.body) throw new Error('Empty MCP response');
  const type = response.headers.get('content-type') ?? '';
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = '', bytes = 0, data = [];
  const parse = text => {
    let message;
    try { message = JSON.parse(text); } catch { return undefined; }
    return message?.id === id && !message.method ? message : undefined;
  };
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      bytes += value.byteLength;
      if (bytes > MAX_BYTES) throw new Error('MCP response exceeds probe limit');
      buffer += decoder.decode(value, { stream: true });
      if (type.includes('text/event-stream')) {
        let newline;
        while ((newline = buffer.indexOf('\n')) !== -1) {
          const line = buffer.slice(0, newline).replace(/\r$/, '');
          buffer = buffer.slice(newline + 1);
          if (line.startsWith('data:')) data.push(line.slice(5).replace(/^ /, ''));
          if (!line && data.length) {
            const result = parse(data.join('\n'));
            data = [];
            if (result) return result;
          }
        }
      }
    }
    buffer += decoder.decode();
    const result = type.includes('text/event-stream') ? parse(data.join('\n')) : parse(buffer);
    if (result) return result;
    throw new Error('Invalid MCP response');
  } finally { await reader.cancel().catch(() => {}); }
}

function headers() {
  const result = new Headers(configuredHeaders);
  result.set('Content-Type', 'application/json');
  result.set('Accept', 'application/json, text/event-stream');
  result.set('MCP-Protocol-Version', version);
  if (session) result.set('Mcp-Session-Id', session);
  else result.delete('Mcp-Session-Id');
  return result;
}

async function request(method, params, id) {
  const body = { jsonrpc: '2.0', method, ...(params ? { params } : {}), ...(id == null ? {} : { id }) };
  let response;
  try { response = await fetch(url, { method: 'POST', headers: headers(), body: JSON.stringify(body), redirect: 'error' }); }
  catch { throw new Error('HTTP MCP connection failed'); }
  if (!response.ok) { await response.body?.cancel(); throw new Error('MCP HTTP status ' + response.status); }
  if (method === 'initialize') session = response.headers.get('Mcp-Session-Id') ?? undefined;
  if (id == null) { await response.body?.cancel(); return; }
  const message = await readResult(response, id);
  if (message.error || !message.result) throw new Error('MCP ' + method + ' failed');
  return message.result;
}

async function discover() {
  const input = JSON.parse(readFileSync(0, 'utf8'));
  url = input.url;
  configuredHeaders = input.headers;
  const init = await request('initialize', { protocolVersion: version, capabilities: {}, clientInfo: { name: 'lyra-probe', version: '1' } }, 1);
  if (!versions.has(init.protocolVersion)) throw new Error('Unsupported MCP protocol version');
  version = init.protocolVersion;
  await request('notifications/initialized');
  if (!init.capabilities?.tools) return [];
  const tools = [];
  let cursor;
  for (let id = 2; id <= 101; id++) {
    const result = await request('tools/list', cursor == null ? {} : { cursor }, id);
    if (!Array.isArray(result.tools) || result.tools.some(tool => typeof tool.name !== 'string')) throw new Error('Invalid tools/list result');
    tools.push(...result.tools.map(tool => tool.name));
    if (tools.length > MAX_TOOLS) throw new Error('MCP server exceeds probe tool limit');
    if (result.nextCursor == null) return tools;
    if (typeof result.nextCursor !== 'string' || result.nextCursor === cursor) throw new Error('Invalid MCP pagination cursor');
    cursor = result.nextCursor;
  }
  throw new Error('MCP server exceeds probe pagination limit');
}

let outcome;
try { outcome = { tools: await discover() }; }
catch (error) {
  // Do not surface arbitrary errors from fetch, headers or remote JSON.
  const message = String(error.message);
  const safe = /^(HTTP MCP connection failed|MCP HTTP status [0-9]{3}|MCP (initialize|tools\/list) failed|Empty MCP response|Invalid MCP response|Invalid tools\/list result|Unsupported MCP protocol version|Invalid MCP pagination cursor|MCP response exceeds probe limit|MCP server exceeds probe (tool|pagination) limit)$/;
  outcome = { error: safe.test(message) ? message : 'HTTP MCP probe failed' };
}
if (session) {
  try {
    const response = await fetch(url, { method: 'DELETE', headers: headers(), redirect: 'error', signal: AbortSignal.timeout(1000) });
    await response.body?.cancel();
  } catch {}
}
process.stdout.write(JSON.stringify(outcome));
