import { element, get } from './ui.js';

export function createProjects() {
  const tree = get('project-tree');
  const projectDialog = get('project-dialog');
  const chatDialog = get('project-chat-dialog');
  const projectForm = get('project-form');
  const chatForm = get('project-chat-form');
  const folderList = get('project-folders');
  const pickFolder = get('pick-project-folder');
  const skill = get('project-skill');
  const prompt = get('project-prompt');
  const composer = get('project-prompt-form');
  const messages = get('project-messages');
  const actions = get('project-actions');
  const sendButton = get('send-project-prompt');
  const drafts = new Map();
  const sending = new Set();
  let catalog;
  let archiveView = false;
  let projectId;
  let chatId;
  let editing;
  let folders = [];
  let installedSkills = [];
  let online = false;
  let run;
  let saving = false;
  let picking = false;
  let catalogRequest = 0;
  let historyRequest = 0;
  let preferredSelection;
  let history = [];

  const selectedProject = () => catalog?.projects.find(project => project.id === projectId);
  const selectedChat = () => selectedProject()?.sessions.find(chat => chat.id === chatId);
  const available = () => online && catalog != null && !saving && !picking;
  const agents = () => online ? Object.values(run?.delegations ?? {}) : [];
  const visibleChats = project => project.sessions.filter(chat => project.archived || chat.archived === archiveView);

  function report(text = '', error = false) {
    const target = projectDialog.open ? 'project-form-message'
      : chatDialog.open ? 'project-chat-form-message' : 'project-message';
    get(target).dataset.error = String(error);
    get(target).textContent = text;
  }

  function updateControls() {
    const ready = available();
    const project = selectedProject();
    const chat = selectedChat();
    const active = chat && (sending.has(chat.id) ||
      agents().some(task => task.project_session_id === chat.id));
    const running = agents();
    const agentStatus = running.length > 2
      ? `Активные агенты · ${running.length} · ${running.map(agent => agent.model).join(', ')}`
      : '';
    const activeAgents = get('project-active-agents');
    activeAgents.hidden = !agentStatus;
    if (activeAgents.textContent !== agentStatus) activeAgents.textContent = agentStatus;
    get('new-project').disabled = !ready;
    get('new-project-chat').hidden = !project || project.archived || archiveView;
    get('new-project-chat').disabled = !ready;
    get('edit-project').hidden = !project || project.archived;
    get('edit-project').disabled = !ready;
    const archiveProject = get('archive-project');
    archiveProject.textContent = project?.archived ? 'Восстановить проект' : 'Архивировать проект';
    archiveProject.disabled = !ready;
    const archiveChat = get('archive-project-chat');
    archiveChat.hidden = !chat || project.archived;
    archiveChat.textContent = chat?.archived ? 'Восстановить чат' : 'Архивировать чат';
    archiveChat.disabled = !ready;
    pickFolder.disabled = !ready;
    pickFolder.textContent = picking ? 'Выбор папки…' : 'Выбрать папку';
    get('cancel-project').disabled = saving || picking;
    get('cancel-project-chat').disabled = saving;
    for (const button of folderList.querySelectorAll('button')) button.disabled = !ready;
    projectForm.querySelector('[type="submit"]').disabled = !ready || !folders.length;
    chatForm.querySelector('[type="submit"]').disabled = !ready;
    composer.hidden = !chat || chat.archived || project.archived;
    sendButton.disabled = !online || !run?.model || composer.hidden || Boolean(active);
    sendButton.textContent = active ? 'Pi работает…' : 'Отправить';
    const model = get('project-chat-model');
    model.textContent = run?.model ?? 'Выбери модель';
    model.title = run?.model ?? '';
  }

  function renderTree(projects) {
    const focused = document.activeElement?.dataset.projectEntry;
    tree.replaceChildren(...projects.map(project => {
      const item = element('li', '');
      const heading = treeButton(project.name, project.id, undefined);
      heading.classList.add('project-tree-heading');
      item.append(heading);
      const chats = visibleChats(project);
      if (chats.length) {
        const list = element('ul', '');
        list.append(...chats.map(chat => {
          const entry = element('li', '');
          entry.append(treeButton(chat.name, project.id, chat.id));
          return entry;
        }));
        item.append(list);
      }
      return item;
    }));
    if (focused) [...tree.querySelectorAll('button')].find(button => button.dataset.projectEntry === focused)?.focus();
    get('show-active-projects').setAttribute('aria-pressed', String(!archiveView));
    get('show-archived-projects').setAttribute('aria-pressed', String(archiveView));
    get('project-tree-empty').textContent = projects.length ? ''
      : archiveView ? 'Архив пуст.' : 'Рабочих проектов пока нет.';
  }

  function treeButton(label, id, session) {
    const button = element('button', label);
    button.type = 'button';
    button.title = label;
    button.dataset.projectEntry = `${id}/${session ?? ''}`;
    button.setAttribute('aria-current', String(id === projectId && session === chatId));
    button.onclick = () => {
      report();
      select({ project: id, chat: session });
    };
    return button;
  }

  function select(preferred) {
    const projects = (catalog?.projects ?? []).filter(project =>
      archiveView ? project.archived || visibleChats(project).length : !project.archived);
    const previousChat = chatId;
    const project = projects.find(project => project.id === (preferred?.project ?? projectId)) ?? projects[0];
    const chats = project ? visibleChats(project) : [];
    const requested = preferred ? preferred.chat : project?.id === projectId ? chatId : chats[0]?.id;
    projectId = project?.id;
    chatId = chats.find(chat => chat.id === requested)?.id;
    const chat = selectedChat();
    if (chatId !== previousChat) {
      actions.open = false;
      history = [];
      prompt.value = drafts.get(chatId) ?? '';
    }

    renderTree(projects);
    get('project-selection').hidden = !project;
    const breadcrumb = get('project-breadcrumb');
    breadcrumb.hidden = !chat;
    breadcrumb.textContent = chat ? project.name : '';
    get('current-project-title').textContent = chat?.name ?? project?.name ?? '';
    const workspace = get('project-chat-workspace');
    workspace.hidden = !chat;
    workspace.textContent = chat?.folder ?? '';
    workspace.title = workspace.textContent;
    const notice = get('project-archive-notice');
    notice.hidden = !project?.archived && !chat?.archived;
    notice.textContent = project?.archived ? 'Проект в архиве. Восстанови проект, чтобы продолжить работу.'
      : chat?.archived ? 'Чат в архиве. Восстанови чат, чтобы продолжить переписку.' : '';
    renderHistory();
    updateControls();
    return refreshHistory();
  }

  function renderHistory() {
    const empty = get('project-empty');
    empty.hidden = history.length > 0;
    empty.textContent = chatId ? 'В этом чате пока нет сообщений.'
      : projectId ? archiveView ? 'Выбери чат, чтобы прочитать переписку.' : 'Выбери чат или начни новый.'
        : archiveView ? 'Здесь появятся архивированные проекты и чаты.' : 'Создай проект, чтобы начать работу.';
    const reading = messages.parentElement;
    const atEnd = reading.scrollHeight - reading.scrollTop - reading.clientHeight < 24;
    messages.hidden = !history.length;
    messages.replaceChildren(...history.map(message => {
      const item = element('li', '');
      item.dataset.role = message.role;
      item.append(element('strong', message.role === 'user' ? 'Ты' : 'Pi'), element('p', message.text));
      return item;
    }));
    if (atEnd) reading.scrollTop = reading.scrollHeight;
  }

  async function refreshHistory() {
    const requestId = ++historyRequest;
    if (!chatId || !online) return;
    try {
      const next = await request(`/projects/${projectId}/sessions/${chatId}`);
      if (requestId !== historyRequest) return;
      history = next;
      renderHistory();
    } catch (error) {
      if (requestId === historyRequest) report(error.message, true);
    }
  }

  async function refresh(preferred) {
    if (preferred) preferredSelection = preferred;
    const requestId = ++catalogRequest;
    try {
      const next = await request('/projects');
      if (requestId !== catalogRequest) return;
      catalog = next;
      const selection = preferredSelection;
      preferredSelection = undefined;
      await select(selection);
    } catch (error) {
      if (requestId === catalogRequest) report(error.message, true);
    }
  }

  function switchView(archived) {
    if (archiveView === archived) return;
    archiveView = archived;
    preferredSelection = undefined;
    actions.open = false;
    report();
    select();
  }

  function openProjectForm(project) {
    editing = project?.id;
    get('project-dialog-title').textContent = project ? 'Настройки проекта' : 'Новый проект';
    get('project-name').value = project?.name ?? '';
    folders = [...(project?.folders ?? [])];
    renderFolders();
    const selectedSkill = project?.search_skill ?? '';
    renderSkills(selectedSkill);
    projectDialog.showModal();
    report();
    request('/skills').then(skills => {
      installedSkills = skills.map(skill => skill.name);
      renderSkills(skill.value);
    }).catch(error => report(error.message, true));
  }

  function renderFolders() {
    get('project-folder-empty').hidden = folders.length > 0;
    folderList.replaceChildren(...folders.map(folder => {
      const row = element('li', '');
      const remove = element('button', 'Убрать');
      remove.type = 'button';
      remove.className = 'unload';
      remove.setAttribute('aria-label', `Убрать папку ${folder}`);
      remove.onclick = () => {
        folders = folders.filter(path => path !== folder);
        renderFolders();
        pickFolder.focus();
      };
      row.append(element('code', folder), remove);
      return row;
    }));
    updateControls();
  }

  function renderSkills(selected) {
    const names = [...new Set([...installedSkills, ...(selected ? [selected] : [])])];
    setOptions(skill, [['', 'Встроенный поиск'], ...names.map(name => [name, name])], selected);
  }

  async function save(path, data, success) {
    if (!available()) return;
    saving = true;
    updateControls();
    report();
    try {
      await success(await request(path, data));
    } catch (error) {
      report(error.message, true);
    } finally {
      saving = false;
      updateControls();
    }
  }

  function archive(chat) {
    const project = selectedProject();
    const target = chat ?? project;
    const archived = !target.archived;
    actions.open = false;
    save('/projects/archive', {
      project_id: project.id,
      project_session_id: chat?.id,
      archived,
    }, async () => {
      if (!archived) archiveView = false;
      await refresh({ project: project.id, chat: archived ? undefined : chat?.id });
      report(chat ? archived ? 'Чат перемещён в архив.' : 'Чат восстановлен.'
        : archived ? 'Проект перемещён в архив.' : 'Проект восстановлен.');
    });
  }

  get('show-active-projects').onclick = () => switchView(false);
  get('show-archived-projects').onclick = () => switchView(true);
  get('new-project').onclick = () => openProjectForm();
  get('edit-project').onclick = () => {
    actions.open = false;
    openProjectForm(selectedProject());
  };
  get('archive-project').onclick = () => archive();
  get('archive-project-chat').onclick = () => archive(selectedChat());
  get('cancel-project').onclick = () => projectDialog.close();
  get('cancel-project-chat').onclick = () => chatDialog.close();
  projectDialog.oncancel = event => { if (picking || saving) event.preventDefault(); };
  chatDialog.oncancel = event => { if (saving) event.preventDefault(); };
  pickFolder.onclick = async () => {
    if (pickFolder.disabled) return;
    picking = true;
    report();
    updateControls();
    try {
      const folder = await request('/projects/pick-folder', null);
      if (folder && !folders.includes(folder)) {
        folders.push(folder);
        renderFolders();
      }
    } catch (error) {
      report(error.message, true);
    } finally {
      picking = false;
      updateControls();
      if (!pickFolder.disabled) pickFolder.focus();
    }
  };

  projectForm.onsubmit = event => {
    event.preventDefault();
    if (!folders.length) return;
    save('/projects', {
      id: editing,
      name: get('project-name').value,
      folders,
      search_skill: skill.value || null,
    }, async project => {
      projectDialog.close();
      archiveView = false;
      await refresh({ project: project.id, chat: chatId });
    });
  };

  let newChatProject;
  get('new-project-chat').onclick = () => {
    newChatProject = selectedProject();
    const folder = get('project-chat-folder');
    setOptions(folder, newChatProject.folders.map(path => [path, path]));
    get('project-chat-name').value = '';
    chatDialog.showModal();
    report();
  };
  chatForm.onsubmit = event => {
    event.preventDefault();
    save('/projects/sessions', {
      project_id: newChatProject.id,
      name: get('project-chat-name').value,
      folder: get('project-chat-folder').value,
    }, async chat => {
      chatDialog.close();
      await refresh({ project: newChatProject.id, chat: chat.id });
    });
  };

  prompt.oninput = () => drafts.set(chatId, prompt.value);
  composer.onsubmit = async event => {
    event.preventDefault();
    if (sendButton.disabled || !prompt.value.trim()) return;
    const project = selectedProject();
    const chat = selectedChat();
    const text = prompt.value;
    sending.add(chat.id);
    report();
    updateControls();
    try {
      const reply = await request('/projects/run', {
        project_id: project.id,
        project_session_id: chat.id,
        model: run.model,
        prompt: text,
        allow_commands: get('project-commands').checked,
      });
      if (drafts.get(chat.id) === text) drafts.delete(chat.id);
      if (chatId === chat.id) {
        prompt.value = drafts.get(chat.id) ?? '';
        if (reply.structuredContent?.truncated) report('Ответ достиг лимита генерации.');
      }
    } catch (error) {
      if (chatId === chat.id) report(error.message, true);
    } finally {
      sending.delete(chat.id);
      updateControls();
      if (chatId === chat.id) refreshHistory();
    }
  };

  return {
    refresh,
    update(connected, state) {
      online = connected;
      run = state;
      updateControls();
    },
  };
}

function setOptions(select, pairs, selected) {
  select.replaceChildren(...pairs.map(([value, label]) => {
    const option = element('option', label);
    option.value = value;
    return option;
  }));
  if (pairs.some(([value]) => value === selected)) select.value = selected;
}

async function request(path, data) {
  const response = await fetch(path, data === undefined ? undefined : {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(data),
  });
  if (!response.ok) throw new Error(await response.text() || 'Запрос к Lyra завершился ошибкой.');
  return response.json();
}
