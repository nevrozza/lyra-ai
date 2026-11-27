import { element, get } from './ui.js';

export function createSkills() {
  const agents = createAgentsEditor();
  const form = get('skill-import');
  const button = get('install-skill');
  const folder = get('skill-folder');
  const file = get('skill-file');
  const pickFolder = get('pick-skill-folder');
  const pickFile = get('pick-skill-file');
  const selection = get('skill-selection');
  const emptySelection = selection.textContent;
  const result = get('skill-result');
  const message = get('skill-catalog-message');
  const list = get('skill-catalog');
  let saving = false;
  let online = false;
  let running = false;
  let revision = 0;
  let files = [];

  function updateControls() {
    pickFolder.disabled = pickFile.disabled = saving || !online;
    button.disabled = saving || !online || !files.length;
    for (const control of list.querySelectorAll('button, input')) {
      control.disabled = saving || !online || (running && control.dataset.remove === 'true');
    }
  }

  function select(input) {
    const chosen = [...input.files];
    input.value = '';
    if (!chosen.length) return;
    files = chosen.map(file => ({
      path: file.webkitRelativePath ? file.webkitRelativePath.split('/').slice(1).join('/') : file.name,
      file,
    }));
    result.textContent = '';
    if (!files.some(file => file.path === 'SKILL.md')) {
      result.dataset.error = 'true';
      result.textContent = 'В корне выбранного скилла нужен файл SKILL.md.';
      files = [];
    } else if (files.length > 2048 || files.reduce((size, entry) => size + entry.file.size, 0) > 16 * 1024 * 1024) {
      result.dataset.error = 'true';
      result.textContent = 'Скилл может содержать до 2048 файлов общим объёмом до 16 МБ.';
      files = [];
    }
    selection.textContent = files.length
      ? chosen[0].webkitRelativePath.split('/')[0] || chosen[0].name
      : emptySelection;
    updateControls();
  }

  async function refreshCatalog() {
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
        entry.dataset.enabled = String(skill.enabled);
        const heading = element('div', '');
        heading.className = 'skill-entry-heading';
        const name = element('code', skill.name);
        const controls = element('div', '');
        controls.className = 'skill-entry-controls';
        const label = element('label', '');
        label.className = 'skill-enable';
        const toggle = element('input', '');
        toggle.type = 'checkbox';
        toggle.checked = skill.enabled;
        toggle.setAttribute('aria-label', 'Включить скилл ' + skill.name);
        toggle.onchange = () => changeSkill(skill, toggle.checked);
        label.append(toggle, element('span', skill.enabled ? 'Включён' : 'Отключён'));
        const remove = element('button', 'Удалить');
        remove.type = 'button';
        remove.className = 'unload skill-remove';
        remove.dataset.remove = 'true';
        remove.title = 'Удалить скилл ' + skill.name;
        remove.onclick = () => {
          if (window.confirm('Удалить скилл «' + skill.name + '» из Lyra?')) changeSkill(skill);
        };
        controls.append(label, remove);
        heading.append(name, controls);
        entry.append(heading, description);
        if (skill.role_template) {
          const note = element('small', 'При отключении расширенные инструкции не загружаются. Базовая роль сохраняется.');
          note.className = 'secondary';
          entry.append(note);
        }
        return entry;
      }));
      updateControls();
    } catch {
      if (request === revision) message.textContent = 'Не удалось загрузить список скиллов.';
    }
  }

  async function changeSkill(skill, enabled) {
    if (!online || saving || (enabled === undefined && running)) return;
    saving = true;
    updateControls();
    result.dataset.error = 'false';
    result.textContent = '';
    try {
      const response = await fetch(enabled === undefined ? '/skills/remove' : '/skills/enabled', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(enabled === undefined ? { name: skill.name } : { name: skill.name, enabled }),
      });
      if (!response.ok) throw new Error(await response.text());
      result.textContent = 'Скилл «' + skill.name + '» ' + (enabled === undefined ? 'удалён.' : enabled ? 'включён.' : 'отключён.');
    } catch (error) {
      result.dataset.error = 'true';
      result.textContent = error instanceof TypeError || !error.message ? 'Не удалось изменить скилл.' : error.message;
    } finally {
      await refreshCatalog();
      saving = false;
      updateControls();
    }
  }

  pickFolder.addEventListener('click', () => folder.click());
  pickFile.addEventListener('click', () => file.click());
  folder.addEventListener('change', () => select(folder));
  file.addEventListener('change', () => select(file));

  form.addEventListener('submit', async event => {
    event.preventDefault();
    if (saving || !online || !files.length) return;
    saving = true;
    result.dataset.error = 'false';
    result.textContent = 'Загрузка…';
    updateControls();
    try {
      const payload = [];
      for (const { path, file } of files) {
        payload.push({ path, data: await readFile(file) });
      }
      const response = await fetch('/skills/upload', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
      });
      if (response.status === 413) throw new Error('Слишком большой объём загрузки.');
      if (!response.ok) throw new Error(await response.text());
      const skill = await response.json();
      files = [];
      selection.textContent = emptySelection;
      result.textContent = `Скилл «${skill.name}» установлен.`;
      await refreshCatalog();
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
    refresh() {
      return Promise.all([refreshCatalog(), agents.refresh()]);
    },
    refreshCatalog,
    setOnline(value, busy = false) {
      online = value;
      running = busy;
      updateControls();
      agents.setOnline(value);
    },
  };
}

function createAgentsEditor() {
  const form = get('agents-form');
  const content = get('agents-content');
  const button = get('save-agents');
  const result = get('agents-result');
  let online = false;
  let busy = false;
  let saved = null;

  function updateControls() {
    content.readOnly = busy || saved === null;
    button.disabled = !online || content.readOnly || content.value === saved;
  }

  async function refresh() {
    if (!online || busy || (saved !== null && content.value !== saved)) return;
    busy = true;
    updateControls();
    try {
      const response = await fetch('/skills/agents');
      if (!response.ok) throw new Error(await response.text());
      content.value = await response.json();
      saved = content.value;
      result.textContent = '';
    } catch (error) {
      result.dataset.error = 'true';
      result.textContent = error instanceof TypeError || !error.message
        ? 'Не удалось загрузить AGENTS.md.'
        : error.message;
    } finally {
      busy = false;
      updateControls();
    }
  }

  content.addEventListener('input', () => {
    result.textContent = '';
    updateControls();
  });
  form.addEventListener('submit', async event => {
    event.preventDefault();
    if (button.disabled) return;
    const draft = content.value;
    busy = true;
    result.dataset.error = 'false';
    result.textContent = 'Сохраняем…';
    updateControls();
    try {
      const response = await fetch('/skills/agents', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(draft),
      });
      if (response.status === 413) throw new Error('Текст AGENTS.md слишком большой.');
      if (!response.ok) throw new Error(await response.text());
      saved = draft;
      result.textContent = 'AGENTS.md сохранён.';
    } catch (error) {
      result.dataset.error = 'true';
      result.textContent = error instanceof TypeError || !error.message
        ? 'Не удалось сохранить AGENTS.md.'
        : error.message;
    } finally {
      busy = false;
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

function readFile(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result.slice(reader.result.indexOf(',') + 1));
    reader.onerror = () => reject(new Error(`Не удалось прочитать файл «${file.name}».`));
    reader.readAsDataURL(file);
  });
}
