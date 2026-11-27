import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

class Node {
  constructor(tag, text = '') {
    this.tag = tag;
    this.children = [];
    this.text = text;
    this.dataset = {};
    this.style = {};
    this.className = '';
    this.classList = { add: name => { this.className += ' ' + name; } };
  }
  append(...nodes) { this.children.push(...nodes); }
  get textContent() { return this.text + this.children.map(node => node.textContent).join(''); }
  set textContent(text) { this.text = text; this.children = []; }
}
const document = {
  createElement: tag => new Node(tag),
  createTextNode: text => new Node('#text', text),
};
const context = vm.createContext({ document, URL });
vm.runInContext(readFileSync(new URL('../vendor/markdown-it.min.js', import.meta.url), 'utf8'), context);
const source = readFileSync(new URL('../js/markdown.js', import.meta.url), 'utf8');
vm.runInContext(source.replace(/^import .*;\s*$/gm, '').replace('export function renderMarkdown', 'function renderMarkdown'), context);
const render = context.renderMarkdown;
const all = (node, tag) => [
  ...(node.tag === tag ? [node] : []),
  ...node.children.flatMap(child => all(child, tag)),
];

test('renders blocks, inline formatting, tables and hard line breaks', () => {
  const rich = render('# Heading\n\n**bold** *italic* ~~del~~\nline\n\n1. one\n   - nested\n\n| A | B |\n| --- | ---: |\n| x | y |\n\n```kotlin\nval x = "<tag>"\n```\n\n> quote');
  for (const tag of ['h1', 'strong', 'em', 's', 'br', 'ol', 'ul', 'table', 'blockquote', 'pre', 'code']) {
    assert.ok(all(rich, tag).length, 'missing ' + tag);
  }
  assert.equal(all(rich, 'pre')[0].dataset.language, 'kotlin');
  assert.equal(all(rich, 'code')[0].textContent, 'val x = "<tag>"\n');
  assert.equal(all(rich, 'th')[1].style.textAlign, 'right');
});
test('untrusted Markdown cannot create executable HTML or fetch images', () => {
  const result = render('<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n[x](javascript:alert%281%29) [relative](/projects) ![remote](https://example.org/track.png) [safe](https://example.org)');
  assert.equal(all(result, 'script').length, 0);
  assert.equal(all(result, 'img').length, 0);
  assert.equal(all(result, 'a').length, 1);
  assert.equal(all(result, 'a')[0].href, 'https://example.org/');
  assert.equal(all(result, 'a')[0].rel, 'noopener noreferrer');
  assert.ok(result.textContent.includes('<script>'));
  assert.ok(result.textContent.includes('Изображение: remote'));
});
test('oversized input and table expansion preserve raw text', () => {
  const long = 'x'.repeat(65537);
  assert.equal(render(long).textContent, long);
  const wide = '|x'.repeat(200) + '\n' + '|---'.repeat(200) + '\n' + ('x\n').repeat(200);
  assert.ok(render(wide).className.includes('project-markdown-plain'));
  assert.equal(render(wide).textContent, wide);
});
