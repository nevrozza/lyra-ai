import { residentRamBytes } from './memory-budget.js';
import { element, get, memoryAmount, memoryLabel, memoryPercent, whole } from './ui.js';

const clock = new Intl.DateTimeFormat('ru-RU', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
const FRESH_FOR_MS = 30_000;

export function metricsFreshness({ closed, failed, sampledAt, now }) {
  if (closed) return ['stopped', 'Остановлено'];
  if (failed) return ['offline', 'Нет связи'];
  if (sampledAt == null) return ['waiting', 'Ожидаем данные'];
  if (now - sampledAt >= FRESH_FOR_MS) return ['stale', 'Данные устарели'];
  return ['fresh', 'Актуально'];
}

function meter(node, value) {
  node.hidden = value == null;
  if (value != null) node.value = value;
}

export function createTelemetry(session, { beginSample, onSample }) {
  const ramExpand = get('ram-expand');
  const ramDialog = get('ram-dialog');
  const ollamaMemoryWarning = get('ollama-memory-warning');
  let polling = false;
  let metricsTimer;
  let freshnessTimer;
  let sampledAt;
  let metricsFailed = false;
  let metricsRefreshPending = false;
  let ramTimer;
  let readingRam = false;
  let detailedRam;

  function gpuCard(gpu) {
    const node = element('div', '');
    node.className = 'gpu';
    const heading = element('div', '');
    heading.className = 'gpu-heading';
    const temperature = element('output', gpu.temperature != null ? `${gpu.temperature} °C` : '—');
    temperature.setAttribute('aria-label', 'Температура GPU');
    heading.append(element('strong', gpu.name), temperature);
    node.append(heading);
    const entries = [
      ['GPU', gpu.load != null ? `${gpu.load}%` : '—', gpu.load],
      ['VRAM', memoryLabel(gpu.vram), memoryPercent(gpu.vram)],
    ];
    for (const [name, label, value] of entries) {
      const row = element('div', '');
      row.className = 'metric';
      row.append(element('span', name), element('output', label));
      if (value != null) {
        const bar = element('meter', '');
        bar.max = 100;
        bar.setAttribute('aria-label', `${gpu.name} ${name}`);
        meter(bar, value);
        row.append(bar);
      }
      node.append(row);
    }
    return node;
  }

  function renderFreshness() {
    clearTimeout(freshnessTimer);
    const now = Date.now();
    const [state, label] = metricsFreshness({ closed: session.closed, failed: metricsFailed, sampledAt, now });
    const node = get('sampled');
    node.dataset.state = state;
    if (node.textContent !== label) node.textContent = label;
    node.title = sampledAt == null ? label : `Последнее обновление: ${clock.format(new Date(sampledAt))}`;
    if (state === 'fresh' && !document.hidden) {
      freshnessTimer = setTimeout(renderFreshness, Math.max(1, FRESH_FOR_MS - (now - sampledAt)));
    }
  }

  function renderMetrics(next) {
    sampledAt = Date.now();
    metricsFailed = false;
    renderFreshness();
    get('cpu-value').textContent = next.cpu_percent == null ? '—' : `${whole.format(next.cpu_percent)}%`;
    renderRamTotal((ramDialog.open && detailedRam) || next.ram);
    get('gpus').replaceChildren(...next.gpus.map(gpuCard));
    get('no-gpu').hidden = next.gpus.length > 0;
    get('no-gpu').textContent = 'Данные NVIDIA GPU недоступны.';
    renderOllamaMemory(next.models);
  }

  function renderOllamaMemory(models) {
    const placements = (models ?? []).flatMap(model => {
      const ram = residentRamBytes(model);
      if (!ram) return [];
      const name = model.context_profile && model.context_length
        ? `${model.name} (${whole.format(model.context_length)} токенов)` : model.name;
      const placement = model.size_vram > 0
        ? `≈ ${memoryAmount(ram)} в ОЗУ, ${memoryAmount(model.size_vram)} в VRAM`
        : `целиком в ОЗУ (≈ ${memoryAmount(ram)})`;
      return [`${name} · ${placement}`];
    });
    const message = placements.length ? ['Память раннеров по отчёту Ollama', ...placements].join('\n') : '';
    if (ollamaMemoryWarning.textContent !== message) ollamaMemoryWarning.textContent = message;
    ollamaMemoryWarning.hidden = !message;
  }

  function renderRamTotal(ram) {
    get('ram-value').textContent = memoryLabel(ram);
    meter(get('ram'), memoryPercent(ram));
  }

  function scheduleMetrics(delay) {
    clearTimeout(metricsTimer);
    if (session.closed || document.hidden) return;
    if (polling) {
      metricsRefreshPending ||= delay === 0;
    } else {
      metricsTimer = setTimeout(readMetrics, delay);
    }
  }

  async function readMetrics() {
    if (session.closed || document.hidden) return;
    polling = true;
    const revision = session.run?.metrics_revision;
    const sample = beginSample();
    try {
      const response = await fetch('/metrics');
      if (!response.ok) throw new Error();
      const next = await response.json();
      if (!session.closed && session.run?.metrics_revision === revision) {
        session.metrics = next;
        renderMetrics(next);
        onSample(sample);
      }
    } catch {
      if (!session.closed && session.run?.metrics_revision === revision) {
        metricsFailed = true;
        renderFreshness();
        renderOllamaMemory(null);
      }
    } finally {
      polling = false;
      const delay = metricsRefreshPending ? 0 : (session.run?.delegated_model || session.run?.benchmark_running ? 5_000 : 15_000);
      metricsRefreshPending = false;
      scheduleMetrics(delay);
    }
  }

  function scheduleRam(delay) {
    clearTimeout(ramTimer);
    if (!session.closed && !document.hidden && ramDialog.open && !readingRam) ramTimer = setTimeout(readRam, delay);
  }

  async function readRam() {
    if (session.closed || document.hidden || !ramDialog.open) return;
    readingRam = true;
    const list = get('ram-list');
    const message = get('ram-message');
    try {
      const response = await fetch('/ram');
      if (!response.ok) throw new Error();
      const usage = await response.json();
      if (session.closed || !ramDialog.open || document.hidden) return;
      const apps = usage.applications;
      detailedRam = usage.physical;
      renderRamTotal(detailedRam ?? session.metrics?.ram);
      renderRamRow('ram-windows', usage.windows_bytes);
      renderRamRow('ram-other', usage.other_process_bytes || null);
      renderRamRow('ram-unattributed', usage.unattributed_bytes);
      list.replaceChildren(...(apps ?? []).map((app) => {
        const row = element('li', '');
        row.append(element('span', app.name), element('output', memoryAmount(app.bytes)));
        return row;
      }));
      message.hidden = Boolean(apps?.length);
      message.textContent = apps === null ? 'Данные по приложениям недоступны.' : 'Нет приложений от 300 МБ.';
    } catch {
      if (session.closed || !ramDialog.open) return;
      detailedRam = undefined;
      renderRamTotal(session.metrics?.ram);
      for (const id of ['ram-windows', 'ram-other', 'ram-unattributed']) renderRamRow(id, null);
      list.replaceChildren();
      message.hidden = false;
      message.textContent = 'Не удалось получить список приложений.';
    } finally {
      readingRam = false;
      scheduleRam(5_000);
    }
  }

  function renderRamRow(id, bytes) {
    get(id).hidden = bytes == null;
    if (bytes != null) get(`${id}-value`).textContent = memoryAmount(bytes);
  }

  ramExpand.addEventListener('click', () => {
    ramDialog.showModal();
    scheduleRam(0);
  });
  ramDialog.addEventListener('close', () => {
    detailedRam = undefined;
    renderRamTotal(session.metrics?.ram);
    clearTimeout(ramTimer);
  });

  function pause() {
    clearTimeout(freshnessTimer);
    clearTimeout(metricsTimer);
    clearTimeout(ramTimer);
  }

  return {
    scheduleMetrics,
    pause,
    resume() {
      renderFreshness();
      scheduleMetrics(0);
      scheduleRam(0);
    },
    showClosed() {
      if (ramDialog.open) ramDialog.close();
      pause();
      renderOllamaMemory(null);
      sampledAt = undefined;
      metricsFailed = false;
      renderFreshness();
    },
    updateControls() { ramExpand.disabled = session.closed; },
  };
}
