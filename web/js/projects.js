import { element, get } from './ui.js';
import { createThinkingControl } from './chat-thinking.js';
import { renderMarkdown } from './markdown.js';

const taskStatuses = {
  queued: 'В очереди', running: 'В работе', completed: 'Готово',
  context_full: 'Ждёт восстановления контекста', failed: 'Ошибка', cancelled: 'Отменена', interrupted: 'Прервана', blocked: 'Заблокирована',
};

function taskStatus(job) {
  if (job.external && job.status === 'queued') return 'Внешний шаг';
  return taskStatuses[job.status] ?? '';
}

export function createProjects() {
  const tree = get('project-tree');
  const projectDialog = get('project-dialog');
  const chatDialog = get('project-chat-dialog');
  const projectForm = get('project-form');
  const chatForm = get('project-chat-form');
  const taskDialog = get('project-task-dialog');
  const taskForm = get('project-task-form');
  const taskModel = get('project-task-model');
  const taskParallel = get('project-task-parallel');
  const taskWritePaths = get('project-task-write-paths');
  function updateTaskWriteScope() {
    get('project-task-write-scope').hidden = !taskParallel.checked;
    taskWritePaths.required = taskParallel.checked;
  }
  taskParallel.onchange = updateTaskWriteScope;
  const folderList = get('project-folders');
  const pickFolder = get('pick-project-folder');
  const prompt = get('project-prompt');
  const composer = get('project-prompt-form');
  const messages = get('project-messages');
  const liveThinking = get('project-live-thinking');
  const liveThinkingText = liveThinking.querySelector('p');
  const entryMenu = get('project-entry-menu');
  const groupDialog = get('project-group-dialog');
  const groupForm = get('project-group-form');
  let groupEditor;
  const sendButton = get('send-project-prompt');
  const stopButton = get('stop-project-prompt');
  const modelSelect = get('project-chat-model');
  const thinking = createThinkingControl();
  const drafts = new Map();
  const chatModels = new Map();
  const chatMode = () => selectedChat()?.mode ?? 'work';
  const sending = new Map();
  const stopRequests = new Set();
  let catalog;
  let archiveView = false;
  let projectId;
  let chatId;
  let editing;
  let folders = [];
  let online = false;
  let run;
  let installedModels = [];
  let modelOptionsKey;
  let saving = false;
  let picking = false;
  let catalogRequest = 0;
  let historyRequest = 0;
  let preferredSelection;
  let history = [];
  let thinkingDelegation;
  let renderedChatId;
  const expandedTools = new Set();

  const selectedProject = () => catalog?.projects.find(project => project.id === projectId);
  const selectedChat = () => selectedProject()?.sessions.find(chat => chat.id === chatId);
  const available = () => online && catalog != null && !saving && !picking;
  const agents = () => online ? Object.values(run?.delegations ?? {}) : [];
  const taskChat = chat => chat && (chat.job ? chat
    : selectedProject()?.sessions.find(owner => owner.job?.chat_session_id === chat.id));
  const chatRun = id => {
    if (!online || id == null) return undefined;
    const chat = selectedProject()?.sessions.find(session => session.id === id);
    const owner = taskChat(chat)?.id;
    return Object.entries(run?.delegations ?? {}).find(([, task]) =>
      task.project_session_id === id || (owner != null && task.project_session_id === owner));
  };
  const visibleChats = project => project.sessions.filter(chat => project.archived || chat.archived === archiveView);
  const pendingTask = chat => ['queued', 'running', 'context_full'].includes(chat?.job?.status);
  const collapsedGroups = new Set();
  const groupMembers = (project, id) => project.sessions.filter(chat => chat.group?.id === id).sort((a, b) => a.id - b.id);
  const chatLabel = (chat, project = selectedProject()) => chat.group && project
    ? `${groupMembers(project, chat.group.id).findIndex(member => member.id === chat.id) + 1}. ${chat.name}`
    : chat.name;
  const errorStatuses = new Set(['failed', 'blocked', 'interrupted', 'cancelled', 'context_full']);
  function chatState(chat, project, activeChats) {
    const owner = chat.job ? chat : project.sessions.find(candidate => candidate.job?.chat_session_id === chat.id);
    const status = owner?.job?.status;
    if (activeChats.has(chat.id) || status === 'running' || status === 'queued') return 'busy';
    if (errorStatuses.has(status) || (!owner && run?.chat_errors?.includes(chat.id))) return 'error';
    return '';
  }
  function chatCompleted(chat, project) {
    const owner = chat.job ? chat : project.sessions.find(candidate => candidate.job?.chat_session_id === chat.id);
    return owner ? owner.job.status === 'completed' : Boolean(run?.chat_completed?.includes(chat.id));
  }
  function stateIcon() {
    const icon = element('span', '');
    icon.className = 'project-tree-status';
    icon.hidden = true;
    return icon;
  }
  function setTreeState(icon, state) {
    const label = state === 'busy' ? 'В работе или в очереди' : state === 'error' ? 'Ошибка или требуется действие' : state === 'done' ? 'Успешно завершено' : '';
    if (icon.dataset.state === state) return;
    icon.dataset.state = state;
    icon.hidden = !state;
    icon.classList.toggle('project-spinner', state === 'busy');
    icon.classList.toggle('project-tree-error', state === 'error');
    icon.classList.toggle('project-tree-done', state === 'done');
    icon.textContent = state === 'error' ? '×' : state === 'done' ? '✓' : '';
    icon.title = label;
    icon.setAttribute('aria-label', label);
    icon.setAttribute('role', 'img');
  }

  function report(text = '', error = false) {
    const target = groupDialog.open ? 'project-group-form-message'
      : taskDialog.open ? 'project-task-form-message'
      : projectDialog.open ? 'project-form-message'
      : chatDialog.open ? 'project-chat-form-message' : 'project-message';
    get(target).dataset.error = String(error);
    get(target).textContent = text;
  }

  function updateControls() {
    const ready = available();
    const project = selectedProject();
    const chat = selectedChat();
    const delegation = chatRun(chat?.id);
    renderLiveThinking(delegation);
    const owner = taskChat(chat);
    const queued = owner?.job?.status === 'queued';
    const contextPaused = owner?.job?.status === 'context_full';
    const taskPending = pendingTask(owner);
    const active = chat && (sending.has(chat.id) || delegation || taskPending);
    const stopping = delegation && (delegation[1].stopping || stopRequests.has(delegation[0]));
    const contextNotice = delegation?.[1].context_notice ?? run?.chat_context?.[chat?.id] ?? run?.chat_context?.[owner?.id];
    const contextStatus = {
      near_limit: 'Контекст почти заполнен. Старые сообщения могут быть сжаты.',
      full: 'Контекст модели заполнен. Увеличь окно контекста, сократи запрос или начни новый чат.',
    }[contextNotice];
    const contextBanner = get('project-context-notice');
    const statusShowsOverflow = contextNotice === 'full' &&
      get('project-message').textContent.startsWith('Контекст модели заполнен.');
    contextBanner.hidden = !contextStatus || statusShowsOverflow;
    if (contextBanner.textContent !== (contextStatus ?? '')) contextBanner.textContent = contextStatus ?? '';
    const running = agents();
    const activeChats = new Set(sending.keys());
    for (const entry of running) {
      activeChats.add(entry.project_session_id);
      for (const p of catalog?.projects ?? []) {
        const child = p.sessions.find(session => session.id === entry.project_session_id)?.job?.chat_session_id;
        if (child != null) activeChats.add(child);
      }
    }
    for (const icon of tree.querySelectorAll('[data-chat-progress], [data-group-progress]')) {
      const p = catalog?.projects.find(candidate => candidate.id === Number(icon.dataset.projectId));
      if (!p) continue;
      if (icon.dataset.groupProgress != null) {
        const members = groupMembers(p, Number(icon.dataset.groupProgress));
        const states = members.map(chat => chatState(chat, p, activeChats));
        const closed = collapsedGroups.has(`${p.id}/group/${icon.dataset.groupProgress}`);
        setTreeState(icon, closed && states.includes('busy') ? 'busy' : '');
      } else {
        const chat = p.sessions.find(candidate => candidate.id === Number(icon.dataset.chatProgress));
        if (!chat) continue;
        const state = chatState(chat, p, activeChats);
        setTreeState(icon, state === 'busy' ? 'busy'
          : !chat.group ? ''
          : state === 'error' ? 'error'
          : chatCompleted(chat, p) ? 'done' : '');
      }
    }
    get('project-progress').hidden = !active;
    const progressText = stopping ? 'Останавливаем…' : contextNotice === 'compacting'
      ? 'Сжимаем историю…' : delegation ? 'Модель отвечает…' : contextPaused ? 'Задача ждёт восстановления контекста…' : queued ? (owner.job.external ? 'Ждём результат от оркестратора…' : 'Задача в очереди…') : 'Отправляем…';
    const progress = get('project-progress-text');
    if (progress.textContent !== progressText) progress.textContent = progressText;
    get('project-empty').hidden = Boolean(active || history.length || sending.has(chatId));
    const agentStatus = running.length > 2
      ? `Активные агенты · ${running.length} · ${running.map(agent => agent.model).join(', ')}`
      : '';
    const activeAgents = get('project-active-agents');
    activeAgents.hidden = !agentStatus;
    if (activeAgents.textContent !== agentStatus) activeAgents.textContent = agentStatus;
    get('new-project').disabled = !ready;
    get('new-project-chat').hidden = !project || project.archived || archiveView;
    get('new-project-chat').disabled = !ready;
    get('new-project-task').hidden = !project || project.archived || archiveView;
    get('new-project-task').disabled = !ready || !(installedModels.length || run?.model);
    for (const button of tree.querySelectorAll('.project-entry-more')) button.disabled = !ready;
    get('cancel-project-group').disabled = saving;
    groupForm.querySelector('[type="submit"]').disabled = !ready;
    pickFolder.disabled = !ready;
    pickFolder.textContent = picking ? 'Выбор папки…' : 'Выбрать папку';
    get('cancel-project').disabled = saving || picking;
    get('cancel-project-chat').disabled = saving;
    get('cancel-project-task-form').disabled = saving;
    taskForm.querySelector('[type="submit"]').disabled = !ready;
    for (const button of folderList.querySelectorAll('button')) button.disabled = !ready;
    projectForm.querySelector('[type="submit"]').disabled = !ready || !folders.length;
    chatForm.querySelector('[type="submit"]').disabled = !ready;
    composer.hidden = !chat || chat.archived || project.archived;
    const working = chatMode() === 'work';
    const chatModeButton = get('project-mode-chat');
    const workModeButton = get('project-mode-work');
    chatModeButton.setAttribute('aria-pressed', String(!working));
    workModeButton.setAttribute('aria-pressed', String(working));
    chatModeButton.disabled = workModeButton.disabled = !ready || composer.hidden || taskPending;
    get('project-command-link').hidden = !working;
    prompt.placeholder = working ? 'Задача для Lyra…' : 'Спроси модель…';
    const model = refreshModels();
    modelSelect.disabled = !online || !model;
    thinking.update(online && run && !composer.hidden ? model : undefined, run);
    sendButton.disabled = !online || saving || !model || !prompt.value.trim() || composer.hidden || Boolean(active);
    sendButton.hidden = Boolean(active);
    stopButton.disabled = (!delegation && !queued && !contextPaused) || Boolean(stopping) || !online || saving;
    stopButton.textContent = stopping ? 'Останавливаем…' : (queued || contextPaused) && !delegation ? 'Отменить задачу' : 'Остановить';
  }

  function refreshModels() {
    const dashboardModel = run?.model;
    const defaultModel = installedModels.length && !installedModels.includes(dashboardModel)
      ? installedModels[0] : dashboardModel;
    const models = [...new Set([defaultModel, ...installedModels].filter(Boolean))];
    const preferred = chatModels.get(chatId) ?? taskChat(selectedChat())?.job?.model;
    const selected = models.includes(preferred) ? preferred : '';
    const key = JSON.stringify([defaultModel, models, selected]);
    if (key !== modelOptionsKey) {
      modelOptionsKey = key;
      const options = [['', defaultModel ? `По умолчанию · ${defaultModel}` : 'Нет доступных моделей']];
      setOptions(modelSelect, [...options, ...models.map(model => [model, model])], selected);
    }
    return selected || defaultModel;
  }

  function renderTree(projects) {
    const focused = document.activeElement?.dataset.projectEntry;
    closeEntryMenu();
    tree.replaceChildren(...projects.map(project => {
      const item = element('li', '');
      const heading = treeButton(project.name, project.id, undefined);
      heading.classList.add('project-tree-heading');
      item.append(entryRow(heading, project));
      const chats = visibleChats(project).sort((a, b) => a.id - b.id);
      const list = element('ul', '');
      const rendered = new Set();
      for (const chat of chats) {
        if (!chat.group) {
          const entry = element('li', '');
          entry.append(entryRow(treeButton(chatLabel(chat, project), project.id, chat.id), project, chat));
          list.append(entry);
          continue;
        }
        const group = chat.group;
        if (rendered.has(group.id)) continue;
        rendered.add(group.id);
        const entry = element('li', '');
        entry.className = 'project-chat-group';
        const key = `${project.id}/group/${group.id}`;
        const toggle = element('button', '');
        toggle.type = 'button';
        toggle.className = 'project-group-heading';
        toggle.title = group.name;
        toggle.dataset.projectEntry = key;
        const label = element('span', group.name);
        const chevron = element('span', '');
        chevron.className = 'project-group-chevron';
        chevron.setAttribute('aria-hidden', 'true');
        const icon = stateIcon();
        icon.dataset.groupProgress = group.id;
        icon.dataset.projectId = project.id;
        toggle.append(label, chevron, icon);
        const children = element('ul', '');
        children.className = 'project-group-chats';
        for (const member of chats.filter(candidate => candidate.group?.id === group.id)) {
          const row = element('li', '');
          row.append(entryRow(treeButton(chatLabel(member, project), project.id, member.id), project, member));
          children.append(row);
        }
        const applyCollapsed = () => {
          const closed = collapsedGroups.has(key);
          children.hidden = closed;
          toggle.setAttribute('aria-expanded', String(!closed));
          chevron.textContent = closed ? '▸' : '▾';
        };
        toggle.onclick = () => {
          if (collapsedGroups.has(key)) collapsedGroups.delete(key);
          else collapsedGroups.add(key);
          applyCollapsed();
          updateControls();
        };
        applyCollapsed();
        entry.append(entryRow(toggle, project, undefined, group), children);
        list.append(entry);
      }
      if (chats.length) item.append(list);
      return item;
    }));
    if (focused) [...tree.querySelectorAll('button')].find(button => button.dataset.projectEntry === focused)?.focus();
    get('show-active-projects').setAttribute('aria-pressed', String(!archiveView));
    get('show-archived-projects').setAttribute('aria-pressed', String(archiveView));
    get('project-tree-empty').textContent = projects.length ? ''
      : archiveView ? 'Архив пуст.' : 'Рабочих проектов пока нет.';
  }

  function closeEntryMenu() {
    if (entryMenu.matches(':popover-open')) entryMenu.hidePopover();
  }

  tree.closest('nav')?.addEventListener('scroll', closeEntryMenu);
  window.addEventListener('resize', closeEntryMenu);

  function entryRow(primary, project, chat, group) {
    const row = element('div', '');
    row.className = 'project-entry-row';
    const more = element('button', '⋯');
    more.type = 'button';
    more.className = 'project-entry-more';
    more.dataset.projectEntry = primary.dataset.projectEntry + '/menu';
    more.setAttribute('aria-label', 'Действия: ' + (chat?.name ?? group?.name ?? project.name));
    more.setAttribute('aria-haspopup', 'true');
    more.title = more.getAttribute('aria-label');
    more.onclick = () => openEntryMenu(more, project, chat, group);
    const icon = primary.querySelector('.project-tree-status');
    row.append(primary, more);
    if (icon) row.append(icon);
    return row;
  }

  function openEntryMenu(anchor, project, chat, group) {
    const wasOpen = entryMenu.matches(':popover-open') && entryMenu.dataset.anchor === anchor.dataset.projectEntry;
    closeEntryMenu();
    if (wasOpen || !available()) return;
    entryMenu.dataset.anchor = anchor.dataset.projectEntry;
    entryMenu.replaceChildren();
    const members = group ? groupMembers(project, group.id) : chat ? [chat] : project.sessions;
    const busy = members.some(member => {
      const owner = member.job ? member : project.sessions.find(candidate => candidate.job?.chat_session_id === member.id);
      return pendingTask(owner) || sending.has(member.id) ||
        agents().some(agent => agent.project_session_id === member.id || agent.project_session_id === owner?.id);
    });
    const archived = group ? archiveView : chat ? chat.archived : project.archived;
    const add = (label, handler, disabled = false) => {
      const button = element('button', label);
      button.type = 'button';
      button.disabled = disabled;
      button.onclick = () => { closeEntryMenu(); handler(); };
      entryMenu.append(button);
    };
    if (!project.archived && !archived) {
      if (group) add('Настройки группы', () => openGroupForm(project, group));
      else if (chat) add('Настройки чата', () => openChatForm(chat, project), busy);
      else add('Настройки проекта', () => openProjectForm(project));
    }
    if (!project.archived || (!chat && !group)) {
      add(archived ? 'Восстановить' : 'Архивировать', () => archive(project, chat, group, !archived), !archived && busy);
    }
    if (!entryMenu.childElementCount) add('Сначала восстановите проект', () => {}, true);
    const rect = anchor.getBoundingClientRect();
    entryMenu.showPopover();
    const width = entryMenu.offsetWidth;
    const height = entryMenu.offsetHeight;
    entryMenu.style.left = Math.max(8, Math.min(rect.right - width, window.innerWidth - width - 8)) + 'px';
    entryMenu.style.top = Math.max(8, Math.min(rect.bottom + 4, window.innerHeight - height - 8)) + 'px';
    entryMenu.querySelector('button:not(:disabled)')?.focus();
  }

  function openGroupForm(project, group) {
    groupEditor = { project, group };
    get('project-group-name').value = group.name;
    groupDialog.showModal();
    report();
    updateControls();
  }
  get('cancel-project-group').onclick = () => groupDialog.close();
  groupDialog.oncancel = event => { if (saving) event.preventDefault(); };
  groupForm.onsubmit = event => {
    event.preventDefault();
    const { project, group } = groupEditor;
    save('/projects/groups/settings', {
      project_id: project.id, group_id: group.id, name: get('project-group-name').value,
    }, async () => {
      groupDialog.close();
      await refresh();
    });
  };

  function treeButton(label, id, session) {
    const button = element('button', '');
    button.type = 'button';
    button.title = label;
    button.dataset.projectEntry = `${id}/${session ?? ''}`;
    button.setAttribute('aria-current', String(id === projectId && session === chatId));
    button.append(element('span', label));
    if (session != null) {
      const icon = stateIcon();
      icon.dataset.chatProgress = session;
      icon.dataset.projectId = id;
      button.append(icon);
    }
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
      closeEntryMenu();
      history = [];
      prompt.value = drafts.get(chatId) ?? '';
    }

    renderTree(projects);
    get('project-selection').hidden = !project;
    const breadcrumb = get('project-breadcrumb');
    breadcrumb.hidden = !chat;
    breadcrumb.textContent = chat ? [project.name, chat.group?.name].filter(Boolean).join(' · ') : '';
    get('current-project-title').textContent = chat ? chatLabel(chat) : project?.name ?? '';
    const jobStatus = get('project-job-status');
    jobStatus.hidden = !chat?.job;
    const dependencies = chat?.job?.after?.map(id => {
      const dependency = project.sessions.find(candidate => candidate.id === id);
      return dependency ? chatLabel(dependency, project) : `#${id}`;
    }).join(', ');
    jobStatus.textContent = chat?.job
      ? [taskStatus(chat.job), chat.job.model, dependencies && `После ${dependencies}`].filter(Boolean).join(' · ') : '';
    const workspace = get('project-chat-workspace');
    workspace.hidden = !chat;
    const owner = taskChat(chat);
    const primary = pendingTask(owner) ? owner.folder : project?.folders[0];
    const workingFolders = chat ? [primary, ...project.folders.filter(folder => folder !== primary)].filter(Boolean) : [];
    workspace.textContent = chat ? [primary, ...(workingFolders.length > 1 ? [`+${workingFolders.length - 1}`] : [])].filter(Boolean).join(' · ') : '';
    workspace.title = workingFolders.join('\n');
    const notice = get('project-archive-notice');
    notice.hidden = !project?.archived && !chat?.archived;
    notice.textContent = project?.archived ? 'Проект в архиве. Восстанови проект, чтобы продолжить работу.'
      : chat?.archived ? 'Чат в архиве. Восстанови чат, чтобы продолжить переписку.' : '';
    renderHistory();
    updateControls();
    return refreshHistory();
  }

  function renderHistory(scrollToEnd = false) {
    const pending = sending.get(chatId);
    const entries = pending && !history.some(message => message.request_id === pending.request_id)
      ? [...history, pending] : history;
    const empty = get('project-empty');
    empty.hidden = entries.length > 0 || Boolean(chatRun(chatId));
    empty.textContent = chatId ? 'В этом чате пока нет сообщений.'
      : projectId ? archiveView ? 'Выбери чат, чтобы прочитать переписку.' : 'Выбери чат или начни новый.'
        : archiveView ? 'Здесь появятся архивированные проекты и чаты.' : 'Создай проект, чтобы начать работу.';
    const reading = messages.parentElement;
    const atEnd = reading.scrollHeight - reading.scrollTop - reading.clientHeight < 24;
    messages.hidden = !entries.length;
    if (renderedChatId !== chatId) {
      expandedTools.clear();
      renderedChatId = chatId;
    }
    messages.replaceChildren(...entries.map(message => {
      const item = element('li', '');
      item.dataset.role = message.role;
      item.dataset.origin = message.origin ?? '';
      appendMessageTime(item, message);
      if (message.tool) {
        const tool = message.tool;
        const details = element('details', '');
        details.className = 'project-tool';
        details.open = expandedTools.has(tool.id);
        details.addEventListener('toggle', () => {
          if (!details.isConnected) return;
          if (details.open) expandedTools.add(tool.id);
          else expandedTools.delete(tool.id);
        });
        const summary = element('summary', '');
        const status = { pending: 'Ожидает результата', ok: 'Готово', error: 'Ошибка' }[tool.status] ?? '';
        summary.append(element('span', tool.name), element('small', status));
        details.dataset.status = tool.status;
        details.append(summary, element('h4', 'Аргументы'), element('pre', tool.arguments || '{}'));
        if (tool.result != null) {
          details.append(element('h4', 'Результат'), element('pre', tool.result || '(пусто)'));
        }
        item.append(details);
        return item;
      }
      const label = message.role === 'assistant' ? (message.model || 'Lyra')
        : { orchestrator: 'Оркестратор', lyra: 'Lyra · Следующий шаг' }[message.origin] ?? '';
      if (label) item.append(element('strong', label));
      if (message.thinking) {
        const details = element('details', '');
        details.className = 'project-thinking';
        details.append(element('summary', 'Размышления модели'), element('p', message.thinking));
        item.append(details);
      }
      if (message.text) item.append(message.role === 'assistant' || message.role === 'user'
        ? renderMarkdown(message.text) : element('p', message.text));
      return item;
    }));
    if (atEnd || scrollToEnd) reading.scrollTop = reading.scrollHeight;
  }

  function renderLiveThinking(delegation) {
    const [id, task] = delegation ?? [];
    const text = task?.thinking ?? '';
    if (thinkingDelegation !== id) {
      thinkingDelegation = id;
      liveThinking.open = true;
    }
    if (liveThinkingText.textContent === text) return;
    const reading = messages.parentElement;
    const atEnd = reading.scrollHeight - reading.scrollTop - reading.clientHeight < 24;
    liveThinkingText.textContent = text;
    liveThinking.hidden = !text.trim();
    if (atEnd) reading.scrollTop = reading.scrollHeight;
  }

  async function refreshHistory() {
    const requestId = ++historyRequest;
    if (!chatId || !online) return;
    try {
      const next = await request(`/projects/${projectId}/sessions/${chatId}`);
      if (requestId === historyRequest) {
        if (JSON.stringify(history) !== JSON.stringify(next)) {
          history = next;
          renderHistory();
        }
      }
      return next;
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
    closeEntryMenu();
    report();
    select();
  }

  function openProjectForm(project) {
    editing = project?.id;
    get('project-dialog-title').textContent = project ? 'Настройки проекта' : 'Новый проект';
    get('project-name').value = project?.name ?? '';
    folders = [...(project?.folders ?? [])];
    renderFolders();
    projectDialog.showModal();
    report();
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

  function archive(project, chat, group, archived) {
    closeEntryMenu();
    save('/projects/archive', {
      project_id: project.id,
      project_session_id: chat?.id,
      group_id: group?.id,
      archived,
    }, async () => {
      if (!archived) archiveView = false;
      await refresh({ project: project.id, chat: archived ? undefined : chat?.id });
      report(archived ? 'Перемещено в архив.' : 'Восстановлено из архива.');
    });
  }

  get('show-active-projects').onclick = () => switchView(false);
  get('show-archived-projects').onclick = () => switchView(true);
  get('new-project').onclick = () => openProjectForm();
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
    }, async project => {
      projectDialog.close();
      archiveView = false;
      await refresh({ project: project.id, chat: chatId });
    });
  };

  let chatEditor;
  function openChatForm(chat, project = selectedProject()) {
    chatEditor = { project, chat };
    const name = get('project-chat-name');
    name.value = chat?.name ?? '';
    name.readOnly = false;
    get('project-chat-dialog-title').textContent = chat ? 'Настройки чата' : 'Новый чат';
    get('save-project-chat').textContent = chat ? 'Сохранить' : 'Создать чат';
    closeEntryMenu();
    chatDialog.showModal();
    report();
  }

  get('new-project-chat').onclick = () => openChatForm();
  let taskProject;
  let taskBatch;
  get('new-project-task').onclick = () => {
    taskProject = selectedProject();
    taskBatch = crypto.randomUUID();
    taskForm.reset();
    updateTaskWriteScope();
    const models = [...new Set([run?.model, ...installedModels].filter(Boolean))];
    setOptions(taskModel, models.map(model => [model, model]), refreshModels());
    taskDialog.showModal();
    report();
  };
  get('cancel-project-task-form').onclick = () => taskDialog.close();
  taskDialog.oncancel = event => {
    if (saving) event.preventDefault();
  };
  taskForm.onsubmit = event => {
    event.preventDefault();
    const project = taskProject;
    save('/projects/tasks', {
      project_id: project.id,
      batch_id: taskBatch,
      tasks: [{
        name: get('project-task-name').value,
        prompt: get('project-task-prompt').value,
        model: taskModel.value,
        parallel: taskParallel.checked,
        ...(taskParallel.checked ? { write_paths: taskWritePaths.value.split(/\r?\n/).map(path => path.trim()).filter(Boolean) } : {}),
      }],
    }, async ([task]) => {
      taskDialog.close();
      await refresh({ project: project.id, chat: task.project_session_id });
    });
  };
  chatForm.onsubmit = event => {
    event.preventDefault();
    const { project, chat } = chatEditor;
    save(chat ? '/projects/sessions/settings' : '/projects/sessions', {
      project_id: project.id,
      ...(chat ? { project_session_id: chat.id } : {}),
      name: get('project-chat-name').value,
    }, async chat => {
      chatDialog.close();
      await refresh({ project: project.id, chat: chat.id });
    });
  };

  modelSelect.onchange = () => {
    chatModels.set(chatId, modelSelect.value);
    updateControls();
  };
  for (const mode of ['chat', 'work']) {
    get(`project-mode-${mode}`).onclick = () => {
      const project = selectedProject();
      const chat = selectedChat();
      if (!chat || chatMode() === mode) return;
      save('/projects/sessions/mode', {
        project_id: project.id,
        project_session_id: chat.id,
        mode,
      }, () => refresh());
    };
  }
  prompt.oninput = () => {
    drafts.set(chatId, prompt.value);
    updateControls();
  };
  stopButton.onclick = async () => {
    if (stopButton.disabled) return;
    const chat = taskChat(selectedChat());
    const delegation = chatRun(chatId);
    if (!delegation && ['queued', 'context_full'].includes(chat?.job?.status)) {
      await save('/projects/tasks/cancel', {
        project_id: selectedProject().id,
        project_session_id: chat.id,
      }, () => refresh());
      return;
    }
    if (!delegation) return;
    const [id, task] = delegation;
    stopRequests.add(id);
    updateControls();
    try {
      await request('/projects/run/stop', {
        delegation_id: Number(id),
        project_session_id: task.project_session_id,
      });
    } catch (error) {
      if (chatRun(chatId)?.[0] === id && error.status !== 404) report(error.message, true);
    } finally {
      stopRequests.delete(id);
      updateControls();
    }
  };

  composer.onsubmit = async event => {
    event.preventDefault();
    if (sendButton.disabled || !prompt.value.trim()) return;
    const project = selectedProject();
    const chat = selectedChat();
    const text = prompt.value;
    const model = refreshModels();
    const requestId = crypto.randomUUID();
    sending.set(chat.id, { role: 'user', origin: 'dashboard', text, request_id: requestId, timestamp: Date.now() });
    drafts.delete(chat.id);
    prompt.value = '';
    report();
    renderHistory(true);
    updateControls();
    let restoreDraft = false;
    try {
      const reply = await request('/projects/run', {
        project_id: project.id,
        project_session_id: chat.id,
        model,
        mode: chatMode(),
        prompt: text,
        request_id: requestId,
      });
      restoreDraft = reply.structuredContent?.stop_reason === 'aborted';
      if (chatId === chat.id) {
        if (restoreDraft) report('Генерация остановлена.');
        else if (reply.structuredContent?.truncated) report('Ответ достиг лимита генерации.');
      }
    } catch (error) {
      restoreDraft = true;
      if (chatId === chat.id) {
        report(error.message, true);
        await refresh();
      }
    } finally {
      const saved = chatId === chat.id ? await refreshHistory() : undefined;
      sending.delete(chat.id);
      if (restoreDraft && !drafts.has(chat.id) && !saved?.some(message => message.request_id === requestId)) {
        drafts.set(chat.id, text);
        if (chatId === chat.id) prompt.value = text;
      }
      if (chatId === chat.id) renderHistory();
      updateControls();
    }
  };

  return {
    refresh,
    update(connected, state, models) {
      const previous = chatRun(chatId);
      online = connected;
      run = state;
      installedModels = models ?? [];
      updateControls();
      const current = chatRun(chatId);
      if (current && (
        current[0] !== previous?.[0] ||
        current[1].history_revision !== previous?.[1].history_revision
      )) {
        void refreshHistory();
      }
    },
  };
}

function appendMessageTime(item, message) {
  if (message.timestamp == null) return;
  const date = new Date(message.timestamp);
  if (Number.isNaN(date.getTime())) return;
  const time = element('time', date.toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit' }));
  time.className = 'project-message-time';
  time.dateTime = date.toISOString();
  const full = date.toLocaleString('ru-RU');
  const end = message.tool?.finished_at == null ? null : new Date(message.tool.finished_at);
  time.title = end && !Number.isNaN(end.getTime())
    ? `Вызов: ${full}\nРезультат: ${end.toLocaleString('ru-RU')}`
    : full;
  time.setAttribute('aria-label', time.title);
  item.append(time);
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
  if (!response.ok) {
    const error = new Error(await response.text() || 'Запрос к Lyra завершился ошибкой.');
    error.status = response.status;
    throw error;
  }
  if (response.status !== 204) return response.json();
}
