import { createActivity } from './js/activity.js';
import { initLayout } from './js/layout.js';
import { createModels } from './js/models.js';
import { createTelemetry } from './js/telemetry.js';
import { get } from './js/ui.js';

const session = { closed: false, online: false, pending: false, run: undefined, metrics: undefined };
const uiRevision = document.querySelector('meta[name="lyra-ui-revision"]').content;
const connection = get('connection');
const connectionLabel = document.createTextNode('Подключение');
connection.lastChild.replaceWith(connectionLabel);
let stream;

const models = createModels(session, {
  refreshControls: updateControls,
  requestMetrics: () => telemetry.scheduleMetrics(0),
});
const activity = createActivity(session, {
  selectedModel: models.displayedModel,
  onPendingChange() {
    models.renderContext();
    updateControls();
  },
  onRunFailed() {
    models.refreshMemoryWarning();
    models.renderContext();
    updateControls();
  },
});
const telemetry = createTelemetry(session, {
  beginSample: models.beginMemorySample,
  onSample(sample) {
    models.acceptMemorySample(sample);
    models.render(session.metrics.models);
  },
});

function updateControls() {
  activity.setEnabled(models.canRun());
  models.updateControls();
  telemetry.updateControls();
}

function connectionState(value, label) {
  session.online = value === 'online';
  connection.dataset.state = value;
  connectionLabel.textContent = label;
  updateControls();
}

function receiveRun(next) {
  const wasChanging = models.isMemoryChanging();
  const justStarted = session.run?.status !== 'running' && next.status === 'running';
  const benchmarkUpdated = session.run?.benchmark_revision !== next.benchmark_revision;
  const metricsUpdated = session.run?.metrics_revision !== next.metrics_revision;
  const delegationChanged = session.run?.delegated_model !== next.delegated_model;
  models.acceptRun();
  session.run = next;
  session.pending = false;
  if (metricsUpdated || (wasChanging && !models.isMemoryChanging())) {
    models.refreshMemoryWarning();
  } else if (justStarted || benchmarkUpdated || delegationChanged) {
    telemetry.scheduleMetrics(0);
  }
  updateControls();
  activity.render(next);
  models.render(session.metrics?.models, benchmarkUpdated);
}

function showClosed() {
  session.closed = true;
  telemetry.showClosed();
  connectionState('closed', 'Остановлена');
  activity.showClosed();
  models.showClosed();
  if (document.hidden) disconnect();
}

function connect() {
  stream = new EventSource('/events');
  stream.onopen = () => {
    if (!session.closed) connectionState('online', 'Подключено');
  };
  stream.onerror = () => {
    if (!session.closed) connectionState('waiting', 'Переподключение');
  };
  stream.onmessage = (event) => {
    const next = JSON.parse(event.data);
    if (next.closed) {
      showClosed();
      return;
    }
    if (next.ui_revision && next.ui_revision !== uiRevision) {
      disconnect();
      window.location.reload();
      return;
    }
    const restarted = session.closed;
    session.closed = false;
    if (restarted) connectionState('online', 'Подключено');
    receiveRun(next);
    if (restarted) telemetry.resume();
  };
}

function disconnect() {
  stream?.close();
  stream = undefined;
}

document.addEventListener('visibilitychange', () => {
  if (document.hidden) {
    // Keep the event stream open until Quit, even with telemetry polling paused.
    if (session.closed) disconnect();
    telemetry.pause();
  } else {
    if (!stream) connect();
    telemetry.resume();
  }
});

setInterval(() => {
  if (session.closed || document.hidden) return;
  activity.tick();
  models.tick();
}, 1_000);

initLayout();
connect();
telemetry.scheduleMetrics(0);
