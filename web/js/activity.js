import { modelResponseError, postModel } from './model-api.js';
import { duration, element, get } from './ui.js';

export function createActivity(session, { onPendingChange, onRunFailed }) {
  const runButton = get('run');
  const timeline = get('timeline');
  let lastTimeline;

  function renderTime() {
    const started = session.run?.started_at;
    get('elapsed').textContent = started == null ? '—' : duration(session.run.elapsed_ms ?? (Date.now() - started));
  }

  function render(next) {
    runButton.textContent = next.status === 'running' ? 'Выполняется…' : 'Запустить проверку';
    const phase = get('phase');
    phase.dataset.status = next.status;
    if (phase.textContent !== next.phase) phase.textContent = next.phase;
    get('workspace-row').hidden = !next.workspace;
    get('workspace').textContent = next.workspace ?? '';
    get('result').hidden = !next.result;
    get('result').textContent = next.result ?? '';
    get('thinking').hidden = !next.thinking;
    if (get('trace').textContent !== next.thinking) get('trace').textContent = next.thinking;
    get('empty').hidden = next.timeline.length > 0;
    const timelineKey = JSON.stringify(next.timeline);
    if (lastTimeline !== timelineKey) {
      lastTimeline = timelineKey;
      timeline.replaceChildren(...next.timeline.map((entry) => {
        const row = element('li', '');
        row.dataset.kind = entry.kind;
        row.append(element('time', `+${duration(entry.at_ms)}`), element('span', entry.text));
        return row;
      }));
    }
    renderTime();
  }

  runButton.addEventListener('click', async () => {
    session.pending = true;
    onPendingChange();
    try {
      const response = await postModel('/run', session.run.model);
      if (!response.ok) throw new Error(modelResponseError(response));
    } catch (error) {
      session.pending = false;
      onRunFailed();
      get('result').hidden = false;
      get('result').textContent = `Не удалось запустить проверку. ${error.message}`;
    }
  });

  function showClosed() {
    get('phase').dataset.status = 'closed';
    get('phase').textContent = 'Lyra закрыта';
    runButton.textContent = 'Запустить проверку';
  }

  return {
    render,
    showClosed,
    setEnabled(enabled) { runButton.disabled = !enabled; },
    tick() {
      if (session.run?.status === 'running') renderTime();
    },
  };
}
