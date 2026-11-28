import { createSessions } from './js/sessions.js';
import { createCommands } from './js/commands.js';
import { createSkills } from './js/skills.js';
import { createMcpServers } from './js/mcp-servers.js';
import { createProjects } from './js/projects.js';
import { initLayout } from './js/layout.js';
import { createModels } from './js/models.js';
import { createTelemetry } from './js/telemetry.js';
import { get, initDialogDismissal } from './js/ui.js';

initDialogDismissal();

const session = { closed: false, online: false, run: undefined, metrics: undefined };
const uiRevision = document.querySelector('meta[name="lyra-ui-revision"]').content;
const connection = get('connection');
const connectionLabel = document.createTextNode('Подключение');
connection.lastChild.replaceWith(connectionLabel);
let stream;

const models = createModels(session, {
  refreshControls: updateControls,
  requestMetrics: () => telemetry.scheduleMetrics(0),
});
const sessions = createSessions();
const commands = createCommands({ openChat: owner => projects.openCommandChat(owner) });
const skills = createSkills();
const mcpServers = createMcpServers();
const projects = createProjects({ onSelection: commands.setSelection });
const telemetry = createTelemetry(session, {
  beginSample: models.beginMemorySample,
  onSample(sample) {
    models.acceptMemorySample(sample);
    models.render(session.metrics.models);
    projects.update(session.online && !session.closed, session.run, session.metrics.installed_models);
  },
});

function updateControls() {
  models.updateControls();
  telemetry.updateControls();
  commands.setOnline(session.online && !session.closed);
  skills.setOnline(session.online && !session.closed, Boolean(session.run?.delegated_model || session.run?.model_action || session.run?.benchmark_running));
  mcpServers.setOnline(session.online && !session.closed);
  projects.update(session.online && !session.closed, session.run, session.metrics?.installed_models);
}

function connectionState(value, label) {
  session.online = value === 'online';
  connection.dataset.state = value;
  connectionLabel.textContent = label;
  updateControls();
}

function receiveRun(next) {
  const wasChanging = models.isMemoryChanging();
  const sessionsUpdated = session.run?.sessions_revision !== next.sessions_revision;
  const commandsUpdated = session.run?.commands_revision !== next.commands_revision;
  const projectsUpdated = session.run?.projects_revision !== next.projects_revision;
  const skillsUpdated = session.run?.skills_revision !== next.skills_revision;
  const benchmarkUpdated = session.run?.benchmark_revision !== next.benchmark_revision;
  const metricsUpdated = session.run?.metrics_revision !== next.metrics_revision;
  const delegationChanged = session.run?.delegated_model !== next.delegated_model;
  models.acceptRun();
  session.run = next;
  if (metricsUpdated || (wasChanging && !models.isMemoryChanging())) {
    models.refreshMemoryWarning();
  } else if (benchmarkUpdated || delegationChanged) {
    telemetry.scheduleMetrics(0);
  }
  updateControls();
  if (sessionsUpdated) sessions.refresh();
  if (commandsUpdated) commands.refresh();
  if (projectsUpdated) projects.refresh();
  if (skillsUpdated) skills.refreshCatalog();
  models.render(session.metrics?.models, benchmarkUpdated);
}

function showClosed() {
  session.closed = true;
  commands.clear();
  telemetry.showClosed();
  connectionState('closed', 'Остановлена');
  models.showClosed();
  if (document.hidden) disconnect();
}

function connect() {
  stream = new EventSource('/events');
  stream.onopen = () => {
    if (!session.closed) {
      connectionState('online', 'Подключено');
      commands.refresh();
      sessions.refresh();
      skills.refresh();
      projects.refresh();
    }
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
    if (restarted) {
      telemetry.resume();
      skills.refresh();
      projects.refresh();
    }
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
    if (!session.closed) commands.refresh();
    telemetry.resume();
  }
});

setInterval(() => {
  if (session.closed || document.hidden) return;
  models.tick();
  if (session.online) sessions.tick();
}, 1_000);

initLayout();
connect();
telemetry.scheduleMetrics(0);
