import '../vendor/markdown-it.min.js';

// Parse CommonMark plus tables and strikethrough, but construct DOM ourselves.
// Never interpret model output as HTML or load images from model-supplied URLs.
const parser = globalThis.markdownit({
  html: false, breaks: true, linkify: false, typographer: false, maxNesting: 20,
});
const MAX_SOURCE = 64 * 1024;
const MAX_TABLE_CELLS = 20000;
const tags = new Set(['p', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'blockquote',
  'ul', 'ol', 'li', 'strong', 'em', 's', 'a', 'table', 'thead', 'tbody', 'tr', 'th', 'td']);

function safeLink(value) {
  if (!/^https?:\/\//i.test(value ?? '')) return null;
  try {
    const url = new URL(value);
    return url.protocol === 'https:' || url.protocol === 'http:' ? url.href : null;
  } catch {
    return null;
  }
}

// Bound the older browser bundle's table auto-completion before parsing.
// A wide header followed by many short rows can otherwise create excessive tokens.
function bounded(source) {
  if (source.length > MAX_SOURCE) return false;
  const lines = source.split('\n');
  if (lines.length > 2000) return false;
  let maxPipes = 0;
  for (const line of lines) {
    const pipes = line.split('|').length - 1;
    maxPipes = Math.max(maxPipes, pipes);
  }
  return maxPipes * lines.length <= MAX_TABLE_CELLS;
}

function appendTokens(parent, tokens) {
  const stack = [parent];
  for (const token of tokens) {
    const current = stack[stack.length - 1];
    if (token.hidden) continue;
    if (token.type === 'inline') {
      appendTokens(current, token.children ?? []);
    } else if (token.type === 'text' || token.type === 'html_inline' || token.type === 'html_block') {
      current.append(document.createTextNode(token.content));
    } else if (token.type === 'softbreak' || token.type === 'hardbreak') {
      current.append(document.createElement('br'));
    } else if (token.type === 'hr') {
      current.append(document.createElement('hr'));
    } else if (token.type === 'code_inline') {
      const code = document.createElement('code');
      code.textContent = token.content;
      current.append(code);
    } else if (token.type === 'fence' || token.type === 'code_block') {
      const pre = document.createElement('pre');
      const code = document.createElement('code');
      code.textContent = token.content;
      const language = token.info.trim().split(/\s+/, 1)[0];
      if (/^[\w+#.-]{1,32}$/.test(language)) pre.dataset.language = language;
      pre.append(code);
      current.append(pre);
    } else if (token.type === 'image') {
      current.append(document.createTextNode(token.content ? '[Изображение: ' + token.content + ']' : '[Изображение]'));
    } else if (token.nesting === 1 && tags.has(token.tag)) {
      const href = token.tag === 'a' ? safeLink(token.attrGet('href')) : null;
      const node = document.createElement(token.tag === 'a' && !href ? 'span' : token.tag);
      if (href) {
        node.href = href;
        node.target = '_blank';
        node.rel = 'noopener noreferrer';
      }
      if (token.tag === 'ol') {
        const start = token.attrGet('start');
        if (/^\d{1,9}$/.test(start ?? '')) node.start = Number(start);
      }
      if (token.tag === 'th' || token.tag === 'td') {
        const align = /^text-align:(left|right|center)$/.exec(token.attrGet('style') ?? '')?.[1];
        if (align) node.style.textAlign = align;
      }
      if (token.tag === 'table') {
        const wrap = document.createElement('div');
        wrap.className = 'markdown-table-scroll';
        wrap.append(node);
        current.append(wrap);
      } else current.append(node);
      stack.push(node);
    } else if (token.nesting === -1 && tags.has(token.tag) && stack.length > 1) {
      stack.pop();
    }
  }
}

export function renderMarkdown(source) {
  const body = document.createElement('div');
  body.className = 'project-markdown';
  try {
    if (!bounded(source)) throw new Error('Markdown render limit');
    appendTokens(body, parser.parse(source, {}));
  } catch {
    // Retain the complete original message if parsing is too expensive or fails.
    body.classList.add('project-markdown-plain');
    body.textContent = source;
  }
  return body;
}
