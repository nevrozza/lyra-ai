import { duration, element, get } from './ui.js';

const clock = new Intl.DateTimeFormat('ru-RU', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
const kinds = { mcp: 'MCP', pi: 'Pi', tool: 'Инструмент Pi', thinking: 'Thinking', answer: 'Ответ' };
const statuses = { running: 'Начато', ok: 'Готово', error: 'Ошибка' };

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

  function render(view) {
    message.hidden = view.sessions.length > 0;
    message.textContent = 'Пока нет вызовов Lyra MCP.';
    const nextListKey = JSON.stringify([view.sessions, view.selected]);
    if (nextListKey !== listKey) {
      const focused = document.activeElement?.dataset.session;
      list.replaceChildren(...view.sessions.map(session => {
        const row = element('li', '');
        const button = element('button', '');
        button.type = 'button';
        button.dataset.session = session.id;
        button.setAttribute('aria-pressed', String(session.id === view.selected));
        const title = element('strong', session.title);
        const status = session.active ? 'Идёт' : session.errors ? 'С ошибками' : 'Завершено';
        const time = element('time', clock.format(new Date(session.started_at)));
        time.dateTime = new Date(session.started_at).toISOString();
        const count = element('span', `MCP · ${session.calls} · ${status}`);
        const meta = element('span', '');
        meta.className = 'session-meta';
        meta.append(time, count);
        button.append(title, meta);
        button.addEventListener('click', () => {
          selected = session.id;
          refresh();
        });
        row.append(button);
        return row;
      }));
      if (focused) list.querySelector(`[data-session="${CSS.escape(focused)}"]`)?.focus();
      listKey = nextListKey;
    }

    get('session-detail').hidden = view.selected == null;
    get('session-omitted').hidden = !view.omitted_events;
    const nextTimelineKey = JSON.stringify([view.selected, view.events]);
    if (nextTimelineKey === timelineKey) return;
    const sameSession = view.selected === shownSession;
    const open = sameSession
      ? new Set([...timeline.querySelectorAll('details[open]')].map(details => details.dataset.event))
      : new Set();
    timeline.replaceChildren(...view.events.map(event => {
      const row = element('li', '');
      row.dataset.kind = event.kind;
      row.dataset.status = event.status ?? '';
      const heading = event.kind === 'mcp' || event.kind === 'tool'
        ? `${kinds[event.kind]} · ${event.title}`
        : event.title === kinds[event.kind] ? event.title : `${kinds[event.kind]} · ${event.title}`;
      const body = element('div', '');
      if (event.kind === 'thinking' || event.kind === 'answer') {
        const details = element('details', '');
        details.dataset.event = event.id;
        details.open = open.has(String(event.id));
        details.append(element('summary', heading), element('pre', event.text));
        body.append(details);
      } else {
        body.append(element('span', heading));
        if (event.status) {
          const status = element('span', statuses[event.status]);
          status.className = 'session-event-status';
          body.append(status);
        }
        if (event.text) {
          const text = element('div', event.text);
          text.className = 'session-event-detail';
          body.append(text);
        }
      }
      row.append(element('time', `+${duration(event.at_ms)}`), body);
      return row;
    }));
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
      message.textContent = 'Не удалось обновить MCP-вызовы.';
    } finally {
      reading = false;
      if (queued) {
        queued = false;
        refresh();
      }
    }
  }

  return { refresh };
}
