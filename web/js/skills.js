import { element, get, iconButton, settingsSwitch, trashIcon, reconcileSettingsRows } from './ui.js';
import { renderMarkdown } from './markdown.js';

export function groupSkills(skills) {
  return {
    custom: skills.filter(skill => !skill.role_template && !skill.builtin),
    builtin: skills.filter(skill => !skill.role_template && skill.builtin),
    roles: skills.filter(skill => skill.role_template),
  };
}

export function createSkills() {
  const agents = createAgentsEditor();
  const form = get('skill-import');
  const importDialog = get('skill-import-dialog');
  const addSkill = get('add-skill');
  const cancelImport = get('cancel-skill-import');
  const importResult = get('skill-import-result');
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
  const builtinList = get('builtin-skill-catalog');
  const roleDetails = get('role-skills');
  const roleList = get('role-skill-catalog');
  const roleCount = get('role-skill-count');
  let saving = false;
  let online = false;
  let running = false;
  let revision = 0;
  let files = [];

  function updateControls() {
    addSkill.disabled = pickFolder.disabled = pickFile.disabled = saving || !online;
    cancelImport.disabled = saving;
    button.disabled = saving || !online || !files.length;
    for (const control of [...list.querySelectorAll('button'), ...builtinList.querySelectorAll('button'), ...roleList.querySelectorAll('button')]) {
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
    importResult.textContent = '';
    if (!files.some(file => file.path === 'SKILL.md')) {
      importResult.dataset.error = 'true';
      importResult.textContent = 'В корне выбранного скилла нужен файл SKILL.md.';
      files = [];
    } else if (files.length > 2048 || files.reduce((size, entry) => size + entry.file.size, 0) > 16 * 1024 * 1024) {
      importResult.dataset.error = 'true';
      importResult.textContent = 'Скилл может содержать до 2048 файлов общим объёмом до 16 МБ.';
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
      const { custom, builtin, roles } = groupSkills(skills);
      message.textContent = '';
      get('custom-skills-empty').hidden = custom.length > 0;
      get('builtin-skills-empty').hidden = builtin.length > 0;
      const count = group => group.filter(skill => skill.enabled).length + ' из ' + group.length;
      get('custom-skill-count').textContent = count(custom);
      get('builtin-skill-count').textContent = count(builtin);
      roleDetails.hidden = !roles.length;
      roleCount.textContent = count(roles);
      const renderSkill = skill => {
        const entry = element('li', '');
        entry.className = 'settings-row';
        entry.dataset.setting = skill.name;
        entry.dataset.enabled = String(skill.enabled);
        const info = element('div', '');
        info.className = 'settings-info';
        info.append(element('strong', skill.name));
        if (skill.description) info.append(element('p', skill.description));
        const controls = element('div', '');
        controls.className = 'settings-actions';
        const toggle = settingsSwitch(skill.name, skill.enabled);
        toggle.onclick = () => changeSkill(skill, !skill.enabled);
        controls.append(toggle);
        if (!skill.builtin) {
          const remove = iconButton('Удалить скилл ' + skill.name, trashIcon);
          remove.dataset.remove = 'true';
          remove.dataset.delete = '';
          remove.onclick = () => {
            if (window.confirm('Удалить скилл «' + skill.name + '» из Lyra?')) changeSkill(skill);
          };
          controls.append(remove);
        }
        entry.append(info, controls);
        return entry;
      };
      reconcileSettingsRows(list, custom.map(renderSkill));
      reconcileSettingsRows(builtinList, builtin.map(renderSkill));
      reconcileSettingsRows(roleList, roles.map(renderSkill));
      updateControls();
    } catch {
      if (request === revision) message.textContent = 'Не удалось загрузить список скиллов.';
    }
  }

  async function changeSkill(skill, enabled) {
    if (!online || saving || (enabled === undefined && (running || skill.builtin))) return;
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

  addSkill.onclick = () => {
    if (!online || saving) return;
    importResult.textContent = '';
    importDialog.showModal();
  };
  cancelImport.onclick = () => { if (!saving) importDialog.close(); };
  importDialog.oncancel = event => { if (saving) event.preventDefault(); };
  importDialog.addEventListener('close', () => {
    files = [];
    selection.textContent = emptySelection;
    importResult.textContent = '';
    updateControls();
  });
  pickFolder.addEventListener('click', () => folder.click());
  pickFile.addEventListener('click', () => file.click());
  folder.addEventListener('change', () => select(folder));
  file.addEventListener('change', () => select(file));

  form.addEventListener('submit', async event => {
    event.preventDefault();
    if (saving || !online || !files.length) return;
    saving = true;
    importResult.dataset.error = 'false';
    importResult.textContent = 'Загрузка…';
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
      result.dataset.error = 'false';
      result.textContent = `Скилл «${skill.name}» установлен.`;
      importDialog.close();
      await refreshCatalog();
    } catch (error) {
      importResult.dataset.error = 'true';
      importResult.textContent = error instanceof TypeError || !error.message
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
  const dialog = get('agents-dialog');
  const open = get('edit-agents');
  const form = get('agents-form');
  const content = get('agents-content');
  const preview = get('agents-preview');
  const editTab = get('agents-edit-tab');
  const previewTab = get('agents-preview-tab');
  const button = get('save-agents');
  const cancel = get('cancel-agents');
  const result = get('agents-result');
  const loadResult = get('agents-load-result');
  let online = false;
  let busy = false;
  let saved = null;

  function updateControls() {
    content.readOnly = !online || busy || saved === null;
    button.disabled = content.readOnly || content.value === saved;
    open.disabled = !online || busy;
    cancel.disabled = busy;
    get('agents-summary').textContent = saved === null ? 'Нет данных'
      : saved.trim() ? saved.split('\n').length + ' строк · ' + new TextEncoder().encode(saved).length.toLocaleString('ru-RU') + ' Б'
      : 'Пустой файл';
  }

  function switchTab(next) {
    content.hidden = next;
    preview.hidden = !next;
    editTab.setAttribute('aria-pressed', String(!next));
    previewTab.setAttribute('aria-pressed', String(next));
    if (next) preview.replaceChildren(renderMarkdown(content.value || 'Нет инструкций.'));
    else preview.replaceChildren();
  }

  async function refresh() {
    if (!online || busy || dialog.open) return;
    busy = true;
    updateControls();
    try {
      const response = await fetch('/skills/agents');
      if (!response.ok) throw new Error(await response.text());
      content.value = await response.json();
      saved = content.value;
      loadResult.textContent = '';
      return true;
    } catch (error) {
      loadResult.dataset.error = 'true';
      loadResult.textContent = error instanceof TypeError || !error.message
        ? 'Не удалось загрузить AGENTS.md.' : error.message;
    } finally {
      busy = false;
      updateControls();
    }
  }

  open.onclick = async () => {
    if (open.disabled) return;
    if (!await refresh() || !online) return;
    content.value = saved;
    result.textContent = '';
    switchTab(false);
    dialog.showModal();
  };
  editTab.onclick = () => switchTab(false);
  previewTab.onclick = () => switchTab(true);
  const canClose = () => !busy && (content.value === saved || window.confirm('Закрыть без сохранения AGENTS.md?'));
  dialog.oncancel = event => { if (!canClose()) event.preventDefault(); };
  cancel.onclick = () => { if (canClose()) dialog.close(); };
  dialog.addEventListener('close', () => {
    content.value = saved ?? '';
    switchTab(false);
    updateControls();
  });

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
        ? 'Не удалось сохранить AGENTS.md.' : error.message;
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
