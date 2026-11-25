import { contextLabel } from './context-settings.js';
import { element, get, memoryAmount, oneDecimal } from './ui.js';

const benchmarkDate = new Intl.DateTimeFormat('ru-RU', { dateStyle: 'medium', timeStyle: 'short' });
export const benchmarkStages = {
  preparing: 'Подготовка',
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
    'Замеров пока нет.');
  const facts = get('benchmark-results');
  facts.hidden = !result;
  if (result) {
    const elapsed = (ms) => ms < 1000 ? `${oneDecimal.format(ms)} мс` : `${oneDecimal.format(ms / 1000)} с`;
    const rate = (tokens) => tokens == null ? '—' : oneDecimal.format(tokens);
    const launch = record.average_launch?.mean_ms;
    const measuredContext = result.measured_context_tokens;
    const rows = [
      ['Контекст прогона', measuredContext != null
        ? (result.context_tokens == null ? `Авто · ${contextLabel(measuredContext)}` : contextLabel(measuredContext))
        : contextLabel(result.context_tokens)],
      ['Среднее время запуска', launch == null ? '—' : elapsed(launch), 'Среднее время загрузки модели в память по холодным запускам'],
      ['Скорость • вход / выход', `${rate(result.prompt_tokens_per_second)} / ${rate(result.generated_tokens_per_second)} ток/с`],
      ['VRAM модели', result.model_vram_bytes == null ? '—' : memoryAmount(result.model_vram_bytes), 'Видеопамять загруженной модели по данным Ollama, включая часть контекста на GPU'],
    ];
    facts.replaceChildren(...rows.map(([label, value, hint]) => {
      const row = document.createElement('div');
      const title = element('dt', label);
      if (hint) title.title = hint;
      row.append(title, element('dd', value));
      return row;
    }));
  }
}
