import { duration, element, get } from './ui.js';

const clock = new Intl.DateTimeFormat('ru-RU', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
const fullClock = new Intl.DateTimeFormat('ru-RU', { dateStyle: 'medium', timeStyle: 'medium' });
const kinds = { mcp: 'MCP', pi: 'Pi', tool: 'Инструмент Pi', thinking: 'Thinking', answer: 'Ответ' };
const statuses = { waiting: 'Ждёт подтверждения', running: 'В работе', ok: 'Готово', error: 'Ошибка' };

export function groupCallEvents(events) {
  const grouped = new Map();
  for (const event of events) {
    if (!grouped.has(event.call)) grouped.set(event.call, []);
    grouped.get(event.call).push(event);
  }
  return grouped;
}

export function formatCallDuration(ms) {
  if (ms == null) return '';
  if (ms < 1000) return ms + ' мс';
  if (ms < 60_000) return (ms / 1000).toFixed(1) + ' с';
  return duration(ms);
}

function timeNode(ms) {
  const date = new Date(ms);
  const time = element('time', clock.format(date));
  time.dateTime = date.toISOString();
  time.title = fullClock.format(date);
  return time;
}

function preserveScroll(container, key, render) {
  const top = container.scrollTop;
  const rect = container.getBoundingClientRect();
  const anchor = top > 8 && [...container.children].find(row => row.getBoundingClientRect().bottom > rect.top);
  const offset = anchor ? anchor.getBoundingClientRect().top - rect.top : 0;
  const id = anchor?.dataset[key];
  render();
  if (top <= 8) container.scrollTop = 0;
  else {
    const next = [...container.children].find(row => id != null && row.dataset[key] === id);
    container.scrollTop = next ? container.scrollTop + next.getBoundingClientRect().top - rect.top - offset : top;
  }
}

export function createSessions() {
  const list = get('session-list');
  const timeline = get('session-events');
  const message = get('session-message');
  let selected;
  let reading = false;
  let queued = false;
  let shownSession;
  let listKey;
  let timelineKey;
  let recentKey;
  let lastRefresh = 0;

  function renderRecent(recent) {
    if (!recent) return;
    const key = JSON.stringify(recent);
    if (recentKey === key) return;
    recentKey = key;
    get('session-recent-total').textContent = recent.total;
    const counts = Object.entries(recent.by_tool).sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
    get('session-recent-tools').replaceChildren(...(counts.length ? counts.map(([name, count]) => {
      const row = element('li', '');
      row.append(element('span', name), element('strong', count));
      return row;
    }) : [element('li', 'Нет вызовов за этот период')]));
  }

  function renderCall(call, events, startedAt, oldRow) {
    const key = JSON.stringify([call, events]);
    if (oldRow?.dataset.content === key) return oldRow;
    const row = element('li', '');
    row.dataset.call = call.id;
    row.dataset.content = key;
    const details = element('details', '');
    details.className = 'session-call';
    details.dataset.status = call.status;
    details.open = oldRow?.firstElementChild?.open ?? false;
    const summary = element('summary', '');
    const title = element('span', '');
    title.className = 'session-call-title';
    const label = element('span', call.tool);
    if (call.model) label.append(element('small', call.model));
    title.append(label);
    const end = element('span', '');
    end.className = 'session-call-end';
    const status = element('span', statuses[call.status] ?? call.status);
    status.className = 'session-event-status';
    end.append(status);
    if (call.duration_ms != null) end.append(element('small', formatCallDuration(call.duration_ms)));
    summary.append(timeNode(call.started_at), title, end);
    details.append(summary);
    const open = new Set([...oldRow?.querySelectorAll('.session-call-event[open]') ?? []].map(node => node.dataset.event));
    function populate() {
      if (!details.open || details.querySelector('.session-call-body')) return;
      const body = element('div', '');
      body.className = 'session-call-body';
      const hasStart = events.some(event => event.kind === 'mcp' && event.status === 'running');
      if (!hasStart) body.append(element('p', 'Часть деталей уже вытеснена из журнала.'));
      for (const event of events) {
        const heading = event.kind === 'mcp'
          ? event.status === 'running' ? 'Запрос' : event.status === 'error' ? 'Ошибка' : 'Результат'
          : [kinds[event.kind], event.title !== kinds[event.kind] ? event.title : '', statuses[event.status]].filter(Boolean).join(' · ');
        const label = clock.format(new Date(startedAt + event.at_ms)) + ' · ' + heading;
        if (event.text) {
          const node = element('details', '');
          node.className = 'session-call-event';
          node.dataset.event = event.id;
          node.dataset.status = event.status ?? '';
          node.open = open.has(String(event.id));
          node.append(element('summary', label), element('pre', event.text));
          body.append(node);
        } else body.append(element('p', label));
      }
      details.append(body);
    }
    details.addEventListener('toggle', populate);
    populate();
    row.append(details);
    return row;
  }

  function render(view) {
    renderRecent(view.recent);
    message.hidden = view.sessions.length > 0;
    message.textContent = 'Пока нет MCP-вызовов оркестратора.';
    const nextListKey = JSON.stringify([view.sessions, view.selected]);
    if (nextListKey !== listKey) {
      const focused = document.activeElement?.dataset.session;
      preserveScroll(list, 'session', () => list.replaceChildren(...view.sessions.map(session => {
        const row = element('li', '');
        row.dataset.session = session.id;
        const button = element('button', '');
        button.type = 'button';
        button.dataset.session = session.id;
        button.setAttribute('aria-pressed', String(session.id === view.selected));
        const title = element('strong', session.title);
        const time = timeNode(session.started_at);
        const count = element('span', 'Вызовов: ' + session.calls);
        const meta = element('span', '');
        meta.className = 'session-meta';
        meta.append(time, count);
        if (session.active) meta.append(element('span', 'В работе: ' + session.active));
        if (session.errors) meta.append(element('span', 'Ошибки: ' + session.errors));
        button.append(title, meta);
        button.addEventListener('click', () => {
          selected = session.id;
          refresh();
        });
        row.append(button);
        return row;
      })));
      if (focused) list.querySelector(`button[data-session="${CSS.escape(focused)}"]`)?.focus({ preventScroll: true });
      listKey = nextListKey;
    }

    get('session-detail').hidden = view.selected == null;
    const omitted = get('session-omitted');
    omitted.hidden = !view.omitted_events && !view.omitted_calls;
    omitted.textContent = view.omitted_calls ? 'Показаны последние вызовы; старые записи и детали ограничены.'
      : 'Показаны последние детали событий.';
    const nextTimelineKey = JSON.stringify([view.selected, view.calls, view.events]);
    if (nextTimelineKey === timelineKey) return;
    const sameSession = view.selected === shownSession;
    const previous = sameSession ? new Map([...timeline.children].map(row => [row.dataset.call, row])) : new Map();
    const grouped = groupCallEvents(view.events);
    const startedAt = view.sessions.find(session => session.id === view.selected)?.started_at ?? 0;
    const rows = view.calls.map(call => renderCall(call, grouped.get(call.id) ?? [], startedAt, previous.get(String(call.id))));
    preserveScroll(timeline, 'call', () => {
      for (const [index, row] of rows.entries()) {
        if (timeline.children[index] !== row) timeline.insertBefore(row, timeline.children[index] ?? null);
      }
      while (timeline.children.length > rows.length) timeline.lastElementChild.remove();
    });
    if (!sameSession) timeline.scrollTop = 0;
    shownSession = view.selected;
    timelineKey = nextTimelineKey;
  }

  async function refresh() {
    if (reading) {
      queued = true;
      return;
    }
    reading = true;
    lastRefresh = Date.now();
    const requested = selected;
    try {
      const response = await fetch(requested == null ? '/sessions' : `/sessions?selected=${requested}`);
      if (!response.ok) throw new Error();
      const view = await response.json();
      if (requested === selected) {
        if (selected != null) selected = view.selected ?? undefined;
        render(view);
      }
    } catch {
      message.hidden = false;
      message.textContent = 'Не удалось обновить MCP-вызовы оркестратора.';
    } finally {
      reading = false;
      if (queued) {
        queued = false;
        refresh();
      }
    }
  }

  return { refresh, tick() { if (Date.now() - lastRefresh >= 15_000) refresh(); } };
}
