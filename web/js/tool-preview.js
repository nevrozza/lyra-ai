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
    return { target, text: code ?? '', label: 'Содержимое файла', ready: code != null };
  }
  return { target, text: raw, label: 'Аргументы' };
}

// The server decodes saved writes before truncating; legacy history falls
// back to its JSON preview. Never read the current file as a substitute.
export function savedWritePreview(tool) {
  if (tool.name !== 'write') return null;
  if (typeof tool.write_preview?.text === 'string') {
    return { target: tool.target || '', text: tool.write_preview.text, truncated: Boolean(tool.write_preview.truncated) };
  }
  const raw = tool.arguments || '';
  const clipped = raw.endsWith('\n… (вывод сокращён)');
  const source = clipped ? raw.slice(0, -'\n… (вывод сокращён)'.length) : raw;
  let args;
  try { args = JSON.parse(source); } catch { /* legacy preview can be clipped */ }
  const text = typeof args?.content === 'string' ? args.content : partialString(source, 'content');
  return text == null ? null : {
    target: tool.target || args?.path || partialString(source, 'path') || '',
    text, truncated: clipped || !args
  };
}

export function codeLanguage(path) {
  const name = path.split(/[\\/]/).pop().toLowerCase();
  if (name === 'dockerfile') return 'Dockerfile';
  if (name === 'makefile') return 'Makefile';
  const ext = name.includes('.') ? name.split('.').pop() : '';
  const names = {js:'JavaScript',jsx:'JSX',mjs:'JavaScript',ts:'TypeScript',tsx:'TSX',html:'HTML',htm:'HTML',css:'CSS',scss:'SCSS',json:'JSON',rs:'Rust',py:'Python',kt:'Kotlin',kts:'Kotlin',java:'Java',swift:'Swift',cpp:'C++',cc:'C++',hpp:'C++',h:'C/C++',c:'C',cs:'C#',go:'Go',md:'Markdown',yaml:'YAML',yml:'YAML',toml:'TOML',xml:'XML',svg:'SVG',sh:'Shell',ps1:'PowerShell',sql:'SQL',gd:'GDScript'};
  return Object.hasOwn(names, ext) ? names[ext] : 'Текст';
}

export function createToolCodeBlock({live = false} = {}) {
  const node = element('div', '');
  node.className = 'tool-code-block';
  const header = element('div', '');
  header.className = 'tool-code-header';
  const name = element('span', '');
  name.className = 'tool-code-filename';
  const language = element('span', '');
  language.className = 'tool-code-language';
  header.append(name, language);
  const pre = element('pre', '');
  pre.dir = 'ltr';
  const code = element('code', '');
  pre.append(code);
  const note = element('p', '');
  note.className = 'tool-code-note';
  node.append(header, pre, note);
  function update(view, follow = false) {
    const target = view.target || '';
    const filename = target.split(/[\\/]/).pop() || 'Файл';
    setText(name, filename);
    name.title = target;
    setText(language, codeLanguage(target));
    const nearEnd = pre.scrollHeight - pre.scrollTop - pre.clientHeight < 24;
    if (setText(code, view.text) && follow && nearEnd) pre.scrollTop = pre.scrollHeight;
    note.hidden = !view.truncated && view.text !== '';
    setText(note, view.truncated ? (live ? 'Предпросмотр ограничен; генерация продолжается.' : 'Показано начало файла; содержимое вызова сокращено.') : live ? 'Ожидаем содержимое файла…' : 'Пустой файл');
  }
  return { node, update };
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
    const fileCode = createToolCodeBlock({live: true});
    fileCode.node.hidden = true;
    body.append(fileCode.node, label, code, note);
    row.append(summary, body);
    return { row, name, state, count, speed, body, label, code, note, chevron, fileCode };
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
      const { row, name, state, count, speed, body, label, code, note, chevron, fileCode } = item;
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
      const write = tool.name === 'write';
      const show = (write || Boolean(view.text)) && tool.phase !== 'executing';
      body.hidden = !show;
      chevron.hidden = !show;
      fileCode.node.hidden = !write || !show;
      label.hidden = code.hidden = !show || write;
      note.hidden = !show || write || !tool.preview_truncated;
      if (write && show) {
        fileCode.update({ ...view, truncated: tool.preview_truncated }, row.open);
      } else if (show) {
        setText(label, view.label);
        const nearEnd = code.scrollHeight - code.scrollTop - code.clientHeight < 24;
        if (setText(code, view.text) && row.open && nearEnd) code.scrollTop = code.scrollHeight;
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
