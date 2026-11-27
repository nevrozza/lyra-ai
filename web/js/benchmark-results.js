import { contextLabel } from './context-settings.js';
import { element, get, memoryAmount, oneDecimal, whole } from './ui.js';

const elapsed = (ms) => ms == null ? '—' : ms < 1000 ? `${oneDecimal.format(ms)} мс` : `${oneDecimal.format(ms / 1000)} с`;
const rate = (tokens) => tokens == null ? '—' : oneDecimal.format(tokens);
const mem = (bytes) => bytes == null ? '—' : memoryAmount(bytes);
const physicalPeak = (point) => point.gpu_memory?.length ? point.gpu_memory.reduce((sum, gpu) => sum + gpu.sampled_peak_bytes, 0) : null;
const coldDelta = (point) => point.cold_isolated && point.gpu_memory?.length ? point.gpu_memory.reduce((sum, gpu) => sum + Math.max(0, gpu.sampled_peak_bytes - gpu.baseline_bytes), 0) : null;
const ramDelta = (point) => point.cold_isolated && point.system_ram?.samples > 0 ? Math.max(0, point.system_ram.sampled_peak_bytes - point.system_ram.baseline_bytes) : null;
const share = (point) => point.ollama_total_bytes > 0 && point.model_vram_bytes != null ? `${oneDecimal.format(Math.min(100, point.model_vram_bytes / point.ollama_total_bytes * 100))}%` : '—';
function renderRows(node, rows) {
  node.replaceChildren(...rows.map(([label, value, hint]) => {
    const row = document.createElement('div');
    const title = element('dt', label);
    if (hint) title.title = hint;
    row.append(title, element('dd', value));
    return row;
  }));
}

const benchmarkDate = new Intl.DateTimeFormat('ru-RU', { dateStyle: 'medium', timeStyle: 'short' });
export const benchmarkStages = {
  preparing: 'Подготовка',
  checking_pi: 'Проверка Pi',
  loading_and_prefill: 'Загрузка и промпт',
  prefill: 'Обработка промпта',
  generating: 'Генерация',
  measuring_memory: 'Замер памяти',
  saving: 'Сохранение',
};

export function renderBenchmark(record, message) {
  const result = record?.latest;
  const summary = get('benchmark-summary');
  const measuredAt = result && Date.parse(result.measured_at);
  summary.textContent = message ?? (result ?
    (Number.isFinite(measuredAt) ? `Результаты от ${benchmarkDate.format(measuredAt)}` : 'Результаты бенчмарка') :
    (record?.memory_profile ? 'Полный профиль сохранён; быстрый замер ещё не выполнен.' : 'Замеров пока нет.'));
  const facts = get('benchmark-results');
  facts.hidden = !result;
  if (result) {
    const launch = record.average_launch?.mean_ms;
    const measuredContext = result.measured_context_tokens;
    const rows = [
      ['Контекст прогона', measuredContext != null
        ? (result.context_tokens == null ? `Авто · ${contextLabel(measuredContext)}` : contextLabel(measuredContext))
        : contextLabel(result.context_tokens)],
      ['Среднее время запуска', launch == null ? '—' : elapsed(launch), 'Среднее время загрузки модели в память по холодным запускам'],
      ['Скорость • вход / выход', `${rate(result.prompt_tokens_per_second)} / ${rate(result.generated_tokens_per_second)} ток/с`],
      ['Размер модели в каталоге', mem(result.model_weight_bytes)],
      ['VRAM / RAM (Ollama)', `${mem(result.model_vram_bytes)} / ${mem(result.model_ram_bytes)}`, 'Оценка Ollama /api/ps; может не отражать физическую память'],
      ['GPU по Ollama', share(result), 'Доля памяти runner по отчёту Ollama, не независимый замер'],
      ['Пик VRAM (GPU)', mem(physicalPeak(result)), 'Сумма выборочных пиков занятости GPU; включает другие процессы'],
      ['Прирост VRAM (холодный)', mem(coldDelta(result)), 'Разность между выборочным пиком и baseline, не счётчик процесса'],
      ['Прирост RAM (система)', mem(ramDelta(result)), 'Выборочный прирост общей занятости RAM, включая другие процессы и изменения кэша ОС'],
      ['Холодная загрузка', result.was_loaded ? 'Прогретая модель' : elapsed(result.load_ms)],
    ];
    renderRows(facts, rows);
  } else facts.replaceChildren();
  const profile = record?.memory_profile;
  get('benchmark-profile').hidden = !profile;
  const points = profile?.points ?? [];
  get('benchmark-profile-note').textContent = !profile ? '' : points.length === 0
    ? 'Замеры прервались до первой точки. Причина в диагностике.'
    : 'В таблице — пиковая память раннера по холодному замеру. Auto сравнивает её с доступным бюджетом.';
  get('benchmark-profile-diagnostics').hidden = !profile?.stopped_reason;
  get('benchmark-profile-reason').textContent = profile?.stopped_reason ?? '';
  get('benchmark-profile-results').replaceChildren(...points.map(point => {
    const row = document.createElement('tr');
    const verified = point.profile_gpu_bytes != null;
    const title = element('th', point.context_tokens == null ? 'Авто' : whole.format(point.context_tokens));
    title.scope = 'row';
    if (point.measured_context_tokens !== point.context_tokens) {
      title.append(element('small', point.measured_context_tokens == null ? 'Факт неизвестен' :
        `Факт: ${whole.format(point.measured_context_tokens)}`));
    }
    const memory = element('td', mem(point.profile_gpu_bytes));
    const speed = element('td', rate(point.generated_tokens_per_second));
    const status = element('td', verified ? 'Подтверждён' : 'Не подтверждён');
    status.dataset.verified = String(verified);
    status.title = `Холодная загрузка: ${elapsed(point.load_ms)}. Прирост VRAM: ${mem(coldDelta(point))}. По Ollama: ${mem(point.model_vram_bytes)} VRAM / ${mem(point.model_ram_bytes)} RAM.`;
    row.append(title, memory, speed, status);
    return row;
  }));
}
