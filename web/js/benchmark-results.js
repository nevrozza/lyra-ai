import { markBenchmarkSelection } from './context-settings.js';
import { element, get, memoryAmount, oneDecimal, whole } from './ui.js';

const elapsed = (ms) => ms == null ? '—' : ms < 1000 ? `${oneDecimal.format(ms)} мс` : `${oneDecimal.format(ms / 1000)} с`;
const rate = (tokens) => tokens == null ? '—' : oneDecimal.format(tokens);
const mem = (bytes) => bytes == null ? '—' : memoryAmount(bytes);
const coldDelta = (point) => point.cold_isolated && point.gpu_memory?.length ? point.gpu_memory.reduce((sum, gpu) => sum + Math.max(0, gpu.sampled_peak_bytes - gpu.baseline_bytes), 0) : null;
const ramDelta = (point) => point.cold_isolated && point.system_ram?.samples > 0 ? Math.max(0, point.system_ram.sampled_peak_bytes - point.system_ram.baseline_bytes) : null;
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
  const profile = record?.memory_profile;
  const points = profile?.points ?? [];
  const summary = get('benchmark-summary');
  const dates = points.map(point => Date.parse(point.measured_at)).filter(Number.isFinite);
  const measuredAt = dates.length ? Math.max(...dates) : NaN;
  summary.textContent = message ?? (profile ?
    (Number.isFinite(measuredAt) ? `Результаты от ${benchmarkDate.format(measuredAt)}` : 'Результаты полного бенчмарка') :
    'Полный бенчмарк ещё не выполнен.');
  const facts = get('benchmark-results');
  facts.hidden = !profile;
  if (profile) {
    const launch = record.average_launch;
    const rows = [
      ['Среднее время запуска', elapsed(launch?.mean_ms),
        'Среднее по завершённым холодным замерам последнего полного прогона, включая остановленный прогон'],
      ['Холодных замеров', whole.format(launch?.samples ?? 0)],
      ['Всего замеров', whole.format(points.length)],
    ];
    renderRows(facts, rows);
  } else facts.replaceChildren();
  get('benchmark-profile').hidden = !profile;
  get('benchmark-profile-note').textContent = !profile ? '' : points.length === 0
    ? 'Замеры прервались до первой точки. Причина в диагностике.'
    : 'Auto сравнивает замеры с бюджетом VRAM и пропускает окна с падением скорости генерации на 30% и больше.';
  get('benchmark-profile-diagnostics').hidden = !profile?.stopped_reason;
  get('benchmark-profile-reason').textContent = profile?.stopped_reason ?? '';
  get('benchmark-profile-results').replaceChildren(...points.map(point => {
    const row = document.createElement('tr');
    const verified = point.profile_gpu_bytes != null;
    const slowdown = point.generation_slowdown_percent;
    if (verified && slowdown == null) row.dataset.verifiedContext = String(point.context_tokens);
    const title = element('th', point.context_tokens == null ? 'Авто' : whole.format(point.context_tokens));
    title.scope = 'row';
    if (point.measured_context_tokens !== point.context_tokens) {
      title.append(element('small', point.measured_context_tokens == null ? 'Факт неизвестен' :
        `Факт: ${whole.format(point.measured_context_tokens)}`));
    }
    const memory = element('td', mem(point.profile_gpu_bytes));
    const speed = element('td', '');
    const input = element('div', `↑ ${rate(point.prompt_tokens_per_second)}`);
    input.title = 'Ввод: обработка промпта, ток/с (без кэшированных токенов)';
    const output = element('div', `↓ ${rate(point.generated_tokens_per_second)}`);
    output.title = 'Вывод: генерация ответа, ток/с';
    speed.append(input, output);
    const launch = element('td', point.was_loaded ? 'Прогрета' : elapsed(point.load_ms));
    const status = element('td', slowdown != null ? `Медленнее на ${slowdown}%` : verified ? 'Подтверждён' : 'Не подтверждён');
    status.dataset.verified = String(verified);
    if (slowdown != null) status.dataset.slowdown = 'true';
    status.title = `Холодная загрузка: ${elapsed(point.load_ms)}. Прирост VRAM: ${mem(coldDelta(point))}. Прирост RAM: ${mem(ramDelta(point))}. По Ollama: ${mem(point.model_vram_bytes)} VRAM / ${mem(point.model_ram_bytes)} RAM.`;
    const choice = element('span', 'Выбор Auto');
    choice.className = 'benchmark-auto-choice';
    choice.hidden = true;
    status.append(choice);
    row.append(title, memory, speed, launch, status);
    return row;
  }));
  markBenchmarkSelection();
}
