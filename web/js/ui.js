export const get = (id) => document.getElementById(id);
export const oneDecimal = new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 1 });
export const whole = new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 0 });

export function element(tag, text) {
  const node = document.createElement(tag);
  node.textContent = text;
  return node;
}

export function duration(ms) {
  const seconds = Math.floor(Math.max(0, ms) / 1000);
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
}

export function memoryAmount(bytes) {
  return bytes >= 2 ** 30 ? `${oneDecimal.format(bytes / 2 ** 30)} ГБ` : `${oneDecimal.format(bytes / 2 ** 20)} МБ`;
}

export function memoryLabel(value) {
  if (!value) return '—';
  const inGigabytes = value.total >= 2 ** 30;
  const unit = inGigabytes ? 2 ** 30 : 2 ** 20;
  const format = inGigabytes ? oneDecimal : whole;
  return `${format.format(value.used / unit)} / ${format.format(value.total / unit)} ${inGigabytes ? 'ГБ' : 'МБ'}`;
}

export function memoryPercent(value) {
  return value?.total ? value.used / value.total * 100 : null;
}
