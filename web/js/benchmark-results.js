import { markBenchmarkSelection } from './context-settings.js';
import { element, get, memoryAmount, oneDecimal, whole } from './ui.js';

const elapsed = (ms) => ms == null ? '—' : ms < 1000 ? `${oneDecimal.format(ms)} мс` : `${oneDecimal.format(ms / 1000)} с`;
const rate = (tokens) => tokens == null ? '—' : oneDecimal.format(tokens);
const mem = (bytes) => bytes == null ? '—' : memoryAmount(bytes);
const coldDelta = (point) => point.cold_isolated && point.gpu_memory?.length ? point.gpu_memory.reduce((sum, gpu) => sum + Math.max(0, gpu.sampled_peak_bytes - gpu.baseline_bytes), 0) : null;
const ramDelta = (point) => point.cold_isolated && point.system_ram?.samples > 0 ? Math.max(0, point.system_ram.sampled_peak_bytes - point.system_ram.baseline_bytes) : null;
export const benchmarkStages = {
  preparing: 'Подготовка',
  checking_pi: 'Проверка работы агента',
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
  summary.textContent = message ?? (profile ? '' : 'Полный бенчмарк ещё не выполнен.');
  summary.hidden = !summary.textContent;
  get('benchmark-results').hidden = !profile;
  get('benchmark-launch-time').textContent = elapsed(record?.average_launch?.mean_ms);
  get('benchmark-count').textContent = `Всего замеров: ${whole.format(points.length)}`;
  get('benchmark-profile').hidden = !profile;
  get('benchmark-profile-note').textContent = 'Auto выбирает контекст по VRAM и скорости генерации.';
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
    const status = element('td', '');
    const failure = point.profile_failure_reason ??
      (slowdown != null ? `Скорость генерации снизилась на ${slowdown}%.` :
        !verified ? 'Не удалось подтвердить размещение модели на GPU.' : null);
    const marker = element('span', failure ? '✕' : '✓');
    const reason = failure ?? 'Замер подтверждён.';
    marker.className = 'benchmark-status-icon';
    marker.dataset.passed = String(!failure);
    marker.tabIndex = 0;
    marker.setAttribute('role', 'img');
    marker.setAttribute('aria-label', reason);
    marker.title = reason;
    status.title = `${reason} Прирост VRAM: ${mem(coldDelta(point))}. Прирост RAM: ${mem(ramDelta(point))}. По Ollama: ${mem(point.model_vram_bytes)} VRAM / ${mem(point.model_ram_bytes)} RAM.`;
    status.append(marker);
    const choice = element('span', 'Выбор Auto');
    choice.className = 'benchmark-auto-choice visually-hidden';
    choice.hidden = true;
    status.append(choice);
    row.append(title, memory, speed, launch, status);
    return row;
  }));
  markBenchmarkSelection();
}
