import { element, get } from './ui.js';

export function createSkills() {
  const form = get('skill-import');
  const path = get('skill-path');
  const button = get('install-skill');
  const result = get('skill-result');
  const message = get('skill-catalog-message');
  const list = get('skill-catalog');
  let saving = false;
  let online = false;
  let revision = 0;

  function updateControls() {
    path.disabled = saving || !online;
    button.disabled = path.disabled || !path.value;
  }

  async function refresh() {
    const request = ++revision;
    try {
      const response = await fetch('/skills');
      if (!response.ok) throw new Error();
      const skills = await response.json();
      if (request !== revision) return;
      message.textContent = skills.length ? '' : 'Пока нет установленных скиллов.';
      list.replaceChildren(...skills.map(skill => {
        const entry = element('li', '');
        const description = element('p', skill.description);
        description.className = 'secondary';
        entry.append(element('code', skill.name), description);
        return entry;
      }));
    } catch {
      if (request === revision) message.textContent = 'Не удалось загрузить список скиллов.';
    }
  }

  path.addEventListener('input', updateControls);
  form.addEventListener('submit', async event => {
    event.preventDefault();
    if (saving || !online || !path.value) return;
    saving = true;
    result.dataset.error = 'false';
    result.textContent = 'Установка…';
    updateControls();
    try {
      const source = path.value;
      const response = await fetch('/skills', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ path: source.startsWith('"') && source.endsWith('"') ? source.slice(1, -1) : source }),
      });
      if (!response.ok) throw new Error(await response.text());
      const skill = await response.json();
      path.value = '';
      result.textContent = `Скилл «${skill.name}» установлен.`;
      await refresh();
    } catch (error) {
      result.dataset.error = 'true';
      result.textContent = error instanceof TypeError || !error.message
        ? 'Не удалось установить скилл.'
        : error.message;
    } finally {
      saving = false;
      updateControls();
    }
  });

  return {
    refresh,
    setOnline(value) {
      online = value;
      updateControls();
    },
  };
}
