import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const context = vm.createContext({});
vm.runInContext(readFileSync(new URL('../js/ui.js', import.meta.url), 'utf8').replace(/export /g, ''), context);

test('near end tolerates small gaps but not reading history', () => {
  const scroller = { scrollHeight: 1000, clientHeight: 400, scrollTop: 570 };
  assert.equal(context.nearChatEnd(scroller), true);
  scroller.scrollTop = 500;
  assert.equal(context.nearChatEnd(scroller), false);
});

test('follow target respects the visible heading of a large expanded block', () => {
  const scroller = { scrollHeight: 1000, clientHeight: 400 };
  assert.equal(context.chatFollowTarget(scroller), 600);
  assert.equal(context.chatFollowTarget(scroller, 480), 480);
  assert.equal(context.chatFollowTarget(scroller, 800), 600);
  assert.equal(context.chatFollowTarget({ scrollHeight: 100, clientHeight: 400 }), 0);
});
