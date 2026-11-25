import { element, get, memoryAmount, memoryLabel, memoryPercent, whole } from './ui.js';

const clock = new Intl.DateTimeFormat('ru-RU', { hour: '2-digit', minute: '2-digit', second: '2-digit' });

function meter(node, value) {
  node.hidden = value == null;
  if (value != null) node.value = value;
}

export function createTelemetry(session, { beginSample, onSample }) {
  const ramExpand = get('ram-expand');
  const ramDialog = get('ram-dialog');
  let polling = false;
  let metricsTimer;
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

  function renderMetrics(next) {
    get('sampled').textContent = clock.format(new Date());
    get('cpu-value').textContent = next.cpu_percent == null ? '—' : `${whole.format(next.cpu_percent)}%`;
    renderRamTotal((ramDialog.open && detailedRam) || next.ram);
    get('gpus').replaceChildren(...next.gpus.map(gpuCard));
    get('no-gpu').hidden = next.gpus.length > 0;
    get('no-gpu').textContent = 'Данные NVIDIA GPU недоступны.';
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
      if (!session.closed && session.run?.metrics_revision === revision) get('sampled').textContent = 'Нет связи';
    } finally {
      polling = false;
      const delay = metricsRefreshPending ? 0 : (session.run?.status === 'running' ? 5_000 : 15_000);
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
    clearTimeout(metricsTimer);
    clearTimeout(ramTimer);
  }

  return {
    scheduleMetrics,
    pause,
    resume() {
      scheduleMetrics(0);
      scheduleRam(0);
    },
    showClosed() {
      if (ramDialog.open) ramDialog.close();
      pause();
      get('sampled').textContent = 'Остановлено';
    },
    updateControls() { ramExpand.disabled = session.closed; },
  };
}
