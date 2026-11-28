import assert from 'node:assert/strict';
import test from 'node:test';
import vm from 'node:vm';
import { readFileSync } from 'node:fs';
const context = vm.createContext({});
vm.runInContext(readFileSync(new URL('../js/tool-preview.js', import.meta.url), 'utf8').replace(/^import .*;\s*$/gm, '').replace(/export function /g, 'function '), context);
test('write preview decodes incomplete JSON without requiring a closed tool call', () => {
  const raw = JSON.stringify({path:'src/ui.js',content:'let x = "hello";\n// мир'}).slice(0,-2);
  const preview = context.toolPreview({name:'write',preview:raw});
  assert.equal(preview.target, 'src/ui.js');
  assert.equal(preview.text, 'let x = "hello";\n// мир');
});
test('partial string tolerates a cut inside an escape', () => {
  const raw = '{"content":"hello' + String.fromCharCode(92) + 'u00';
  assert.equal(context.partialString(raw,'content'), 'hello');
});
test('paths inside escaped content are not mistaken for a path field', () => {
  const raw = JSON.stringify({content:'example: {"path":"wrong"}',path:'right'});
  assert.equal(context.partialString(raw,'path'), 'right');
});

test('saved write uses decoded original content and preserves empty files', () => {
  const text = 'const x = "hello";\n\t// мир';
  const view = context.savedWritePreview({name:'write',target:'src/a.ts',write_preview:{text,truncated:false}});
  assert.equal(view.text,text);
  assert.equal(view.target,'src/a.ts');
  assert.equal(view.truncated,false);
  assert.equal(context.savedWritePreview({name:'write',write_preview:{text:'',truncated:false}}).text,'');
  assert.equal(context.savedWritePreview({name:'read',arguments:'{}'}),null);
});
test('legacy truncated writes show code instead of broken JSON', () => {
  const args = JSON.stringify({path:'src/a.js',content:'line 1\nline 2'}).slice(0,-3) + '\n… (вывод сокращён)';
  const view = context.savedWritePreview({name:'write',arguments:args});
  assert.equal(view.text,'line 1\nline ');
  assert.equal(view.truncated,true);
});
test('language is derived from filename, and pending writes do not show JSON', () => {
  assert.equal(context.codeLanguage('C:\\project\\App.tsx'),'TSX');
  assert.equal(context.codeLanguage('src/main.rs'),'Rust');
  assert.equal(context.toolPreview({name:'write',preview:'{"path":"a.js"'}).text,'');
});
