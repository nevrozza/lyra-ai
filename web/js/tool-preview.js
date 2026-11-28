import { element } from './ui.js';

// JSON chunks can end inside a string, an escape, or a Unicode escape.
// Recover only a string value from a bounded argument preview. This is
// presentation only: it never supplies arguments to an actual tool call.
export function partialString(raw, field) {
  const marker = new RegExp('(?:^|[,{])\\s*"' + field + '"\\s*:\\s*"');
  const match = marker.exec(raw);
  if (!match) return undefined;
  const start = match.index + match[0].length;
  let end = start;
  let escaped = false;
  for (; end < raw.length; end++) {
    const ch = raw[end];
    if (escaped) { escaped = false; continue; }
    if (ch === '\\') { escaped = true; continue; }
    if (ch === '"') {
      try { return JSON.parse(raw.slice(start - 1, end + 1)); }
      catch { return undefined; }
    }
  }
  const prefix = raw.slice(start, end);
  for (let cut = 0; cut <= Math.min(6, prefix.length); cut++) {
    try { return JSON.parse('"' + prefix.slice(0, prefix.length - cut) + '"'); }
    catch { /* the stream may end in an incomplete escape */ }
  }
  return undefined;
}

const format = new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 1 });
const size = bytes => bytes < 1024 ? bytes + ' Б' : format.format(bytes / 1024) + ' КБ';

export function toolPreview(tool) {
  const raw = tool.preview || '';
  const target = tool.target || partialString(raw, 'path') || '';
  if (tool.name === 'write') {
    const code = partialString(raw, 'content');
    return { target, text: code ?? raw, label: code == null ? 'Аргументы' : 'Содержимое файла' };
  }
  return { target, text: raw, label: 'Аргументы' };
}

function setText(node, value) {
  if (node.textContent === value) return false;
  node.textContent = value;
  return true;
}

export function createLiveTools(container, changed) {
  let delegationId;
  let rows = new Map();
  let lastKey;
  function createRow() {
    const row = element('details', '');
    row.className = 'live-tool';
    row.open = true;
    const summary = element('summary', '');
    const icon = element('span', '');
    icon.className = 'live-tool-icon';
    icon.setAttribute('aria-hidden', 'true');
    const title = element('span', '');
    title.className = 'live-tool-title';
    const name = element('span', '');
    name.className = 'live-tool-name';
    const state = element('span', '');
    state.className = 'live-tool-state';
    title.append(name, state);
    const metrics = element('span', '');
    metrics.className = 'live-tool-metrics';
    const count = element('span', '');
    const speed = element('span', '');
    metrics.append(count, speed);
    const chevron = element('span', '');
    chevron.className = 'disclosure-chevron';
    chevron.setAttribute('aria-hidden', 'true');
    summary.append(icon, title, metrics, chevron);
    const body = element('div', '');
    body.className = 'live-tool-body';
    const label = element('p', '');
    const code = element('pre', '');
    code.dir = 'ltr';
    const note = element('p', 'Предпросмотр ограничен; генерация продолжается.');
    note.hidden = true;
    body.append(label, code, note);
    row.append(summary, body);
    return { row, name, state, count, speed, body, label, code, note, chevron };
  }
  function render(delegation) {
    const [id, task] = delegation ?? [];
    const tools = task?.live_tools ?? [];
    const key = JSON.stringify([id, tools]);
    if (key === lastKey) return;
    lastKey = key;
    if (id !== delegationId) {
      delegationId = id;
      container.replaceChildren();
      rows.clear();
    }
    const next = new Map();
    for (const [index, tool] of tools.entries()) {
      const item = rows.get(tool.id) ?? createRow();
      const { row, name, state, count, speed, body, label, code, note, chevron } = item;
      const view = toolPreview(tool);
      row.dataset.phase = tool.phase;
      const title = tool.name + (view.target ? ' · ' + view.target : '');
      setText(name, title);
      name.title = title;
      setText(state, tool.phase === 'generating' ? (tool.name === 'write' ? 'Генерация содержимого' : 'Генерация аргументов')
        : tool.phase === 'queued' ? 'Аргументы готовы · ожидает выполнения' : 'Выполнение инструмента');
      const elapsed = tool.generation_ms;
      setText(count, tool.argument_bytes > 0 ? size(tool.argument_bytes) + (elapsed != null ? ' · ' + format.format(elapsed / 1000) + ' с' : '') : '');
      const rate = elapsed >= 250 && tool.argument_bytes > 0 ? size(Math.round(tool.argument_bytes * 1000 / elapsed)) + '/с' : '';
      setText(speed, tool.phase === 'generating' ? rate : '');
      const show = Boolean(view.text) && tool.phase !== 'executing';
      body.hidden = !show;
      chevron.hidden = !show;
      if (show) {
        setText(label, view.label);
        const nearEnd = code.scrollHeight - code.scrollTop - code.clientHeight < 24;
        if (setText(code, view.text) && row.open && nearEnd) code.scrollTop = code.scrollHeight;
        note.hidden = !tool.preview_truncated;
      } else {
        setText(code, '');
      }
      next.set(tool.id, item);
      if (container.children[index] !== row) container.insertBefore(row, container.children[index] ?? null);
    }
    for (const [key, item] of rows) if (!next.has(key)) item.row.remove();
    rows = next;
    container.hidden = tools.length === 0;
    changed();
  }
  return { render };
}
