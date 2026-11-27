import { element, get } from './ui.js';
import { createThinkingControl } from './chat-thinking.js';
import { renderMarkdown } from './markdown.js';

const taskStatuses = {
  queued: 'В очереди', running: 'В работе', completed: 'Готово',
  context_full: 'Ждёт восстановления контекста', output_limit: 'Ждёт продолжения после лимита генерации', failed: 'Ошибка', cancelled: 'Отменена', interrupted: 'Прервана', blocked: 'Заблокирована',
};

function taskStatus(job) {
  if (job.external && job.status === 'queued') return 'Внешний шаг';
  return taskStatuses[job.status] ?? '';
}

// Sort top-level entries by creation ID, but preserve the numbered order of
// steps inside each group. A new step does not move its entire group.
export function compareTreeChats(a, b) {
  return (b.group?.id ?? b.id) - (a.group?.id ?? a.id) || a.id - b.id;
}

// Preserve chronological order: tool runs, thinking-only turns and skill
// notices can share a compact activity block, but never cross visible messages.
function isActivity(message) {
  return Boolean(message.tool || (message.role === 'assistant' && !message.text && message.thinking)
    || (message.role === 'notice' && message.text?.startsWith('Использован скилл: ')));
}

export function groupChatHistory(entries) {
  const blocks = [];
  let activity;
  for (const message of entries) {
    if (isActivity(message)) {
      if (!activity) {
        activity = { kind: 'activity', entries: [] };
        blocks.push(activity);
      }
      activity.entries.push(message);
    } else {
      activity = undefined;
      blocks.push({ kind: 'message', message });
    }
  }
  return blocks;
}

export function activityOpen(active, manual) {
  return manual ?? active;
}

export function createProjects({ onSelection = () => {} } = {}) {
  const tree = get('project-tree');
  const projectDialog = get('project-dialog');
  const chatDialog = get('project-chat-dialog');
  const projectForm = get('project-form');
  const chatForm = get('project-chat-form');
  const taskDialog = get('project-task-dialog');
  const taskForm = get('project-task-form');
  const taskModel = get('project-task-model');
  const taskParallel = get('project-task-parallel');
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
  const contextDialog = get('group-context-dialog');
  const contextForm = get('group-context-form');
  const contextBrief = get('group-context-brief');
  let contextEditor;
  let contextRequest = 0;
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
  const expandedDetails = new Set();
  const activityOverrides = new Map();

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
  const visibleChats = project => project.sessions
    .filter(chat => project.archived || chat.archived === archiveView)
    .sort(compareTreeChats);
  const pendingTask = chat => ['queued', 'running', 'context_full', 'output_limit'].includes(chat?.job?.status);
  const collapsedGroups = new Set();
  const collapsedProjects = new Set();
  const groupMembers = (project, id) => project.sessions.filter(chat => chat.group?.id === id).sort((a, b) => a.id - b.id);
  const chatLabel = (chat, project = selectedProject()) => chat.group && project
    ? `${groupMembers(project, chat.group.id).findIndex(member => member.id === chat.id) + 1}. ${chat.name}`
    : chat.name;
  const errorStatuses = new Set(['failed', 'blocked', 'interrupted', 'context_full', 'output_limit']);
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
    const target = contextDialog.open ? 'group-context-message' : groupDialog.open ? 'project-group-form-message'
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
    const outputPaused = owner?.job?.status === 'output_limit';
    const recoveryPaused = contextPaused || outputPaused;
    const taskPending = pendingTask(owner);
    const active = chat && (sending.has(chat.id) || delegation || taskPending);
    const workingModel = delegation?.[1].model ?? sending.get(chat?.id)?.model;
    const workingModelNode = get('project-working-model');
    const workingName = get('project-working-model-name');
    workingModelNode.hidden = !workingModel;
    if (workingName.textContent !== (workingModel ?? '')) workingName.textContent = workingModel ?? '';
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
    for (const icon of tree.querySelectorAll('[data-chat-progress], [data-group-progress], [data-project-progress]')) {
      const p = catalog?.projects.find(candidate => candidate.id === Number(icon.dataset.projectId));
      if (!p) continue;
      if (icon.dataset.projectProgress != null) {
        const busy = visibleChats(p).some(chat => chatState(chat, p, activeChats) === 'busy');
        setTreeState(icon, collapsedProjects.has(p.id) && busy ? 'busy' : '');
      } else if (icon.dataset.groupProgress != null) {
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
      ? 'Сжимаем историю…' : delegation ? 'Модель отвечает…' : contextPaused ? 'Задача ждёт восстановления контекста…' : outputPaused ? 'Задача ждёт продолжения после лимита генерации…' : queued ? (owner.job.external ? 'Ждём результат от оркестратора…' : 'Задача в очереди…') : 'Отправляем…';
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
    updateContextControls();
    for (const button of tree.querySelectorAll('.group-context-entry')) button.disabled = !ready;
    get('cancel-project-group').disabled = saving;
    groupForm.querySelector('[type="submit"]').disabled = !ready;
    pickFolder.disabled = !ready;
    pickFolder.textContent = picking ? 'Выбор папки…' : 'Выбрать папку';
    get('cancel-project').disabled = saving || picking;
    get('cancel-project-chat').disabled = saving;
    get('cancel-project-task-form').disabled = saving;
    taskForm.querySelector('[type="submit"]').disabled = !ready;
    for (const button of folderList.querySelectorAll('button')) button.disabled = !ready;
    projectForm.querySelector('[type="submit"]').disabled = !ready;
    chatForm.querySelector('[type="submit"]').disabled = !ready;
    composer.hidden = !chat || chat.archived || project.archived;
    const working = chatMode() === 'work';
    get('project-mode-control').hidden = !chat;
    const chatModeButton = get('project-mode-chat');
    const workModeButton = get('project-mode-work');
    chatModeButton.setAttribute('aria-pressed', String(!working));
    workModeButton.setAttribute('aria-pressed', String(working));
    chatModeButton.disabled = workModeButton.disabled = !ready || composer.hidden || taskPending;
    prompt.placeholder = working ? 'Задача для Lyra…' : 'Спроси модель…';
    const model = refreshModels();
    modelSelect.disabled = !online || !model;
    thinking.update(online && run && !composer.hidden ? model : undefined, run);
    sendButton.disabled = !online || saving || !model || !prompt.value.trim() || composer.hidden || Boolean(active);
    sendButton.hidden = Boolean(active);
    stopButton.disabled = (!delegation && !queued && !recoveryPaused) || Boolean(stopping) || !online || saving;
    stopButton.textContent = stopping ? 'Останавливаем…' : (queued || recoveryPaused) && !delegation ? 'Отменить задачу' : 'Остановить';
  }

  function refreshModels() {
    const dashboardModel = run?.model;
    const defaultModel = installedModels.length && !installedModels.includes(dashboardModel)
      ? installedModels[0] : dashboardModel;
    const models = [...new Set([defaultModel, ...installedModels].filter(Boolean))];
    const preferred = chatModels.get(chatId) ?? taskChat(selectedChat())?.job?.model;
    const selected = models.includes(preferred) ? preferred : models[0];
    if (chatId != null && selected) chatModels.set(chatId, selected);
    const key = JSON.stringify([models, selected]);
    if (key !== modelOptionsKey) {
      modelOptionsKey = key;
      setOptions(modelSelect, models.length ? models.map(model => [model, model]) :
        [['', 'Нет доступных моделей']], selected ?? '');
    }
    return selected;
  }

  function renderTree(projects) {
    const focused = document.activeElement?.dataset.projectEntry;
    closeEntryMenu();
    tree.replaceChildren(...projects.map(project => {
      const item = element('li', '');
      const heading = treeButton(project.name, project.id, undefined);
      heading.classList.add('project-tree-heading');
      const icon = stateIcon();
      icon.dataset.projectProgress = project.id;
      icon.dataset.projectId = project.id;
      heading.append(icon);
      const row = entryRow(heading, project);
      item.append(row);
      const chats = visibleChats(project);
      const list = element('ul', '');
      const chevron = element('span', '');
      chevron.className = 'project-group-chevron';
      chevron.setAttribute('aria-hidden', 'true');
      heading.append(chevron);
      const applyProjectCollapsed = () => {
        const closed = collapsedProjects.has(project.id);
        list.hidden = closed;
        chevron.hidden = !chats.length;
        if (chats.length) heading.setAttribute('aria-expanded', String(!closed));
        else heading.removeAttribute('aria-expanded');
        chevron.textContent = closed ? '▸' : '▾';
      };
      heading.onclick = () => {
        // Select a different project first; clicking the current project
        // toggles its children without closing the conversation.
        if (project.id !== projectId) {
          collapsedProjects.delete(project.id);
          report();
          select({ project: project.id, chat: undefined });
          return;
        }
        if (!chats.length) return;
        if (collapsedProjects.has(project.id)) collapsedProjects.delete(project.id);
        else collapsedProjects.add(project.id);
        applyProjectCollapsed();
        updateControls();
      };
      applyProjectCollapsed();
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
        const contextRow = element('li', '');
        const contextButton = element('button', '');
        contextButton.type = 'button';
        contextButton.className = 'group-context-entry';
        contextButton.dataset.projectEntry = key + '/context';
        contextButton.title = 'Общий контекст группы';
        contextButton.append(element('span', 'CONTEXT.md'));
        contextButton.onclick = () => openGroupContext(project, group);
        contextRow.append(contextButton);
        children.append(contextRow);
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
    const add = (label, handler, disabled = false, destructive = false) => {
      const button = element('button', label);
      button.type = 'button';
      button.disabled = disabled;
      if (destructive) button.classList.add('project-delete-action');
      button.onclick = () => { closeEntryMenu(); handler(); };
      entryMenu.append(button);
    };
    if (group) add('Общий контекст', () => openGroupContext(project, group));
    if (!project.archived && !archived) {
      if (group) add('Настройки группы', () => openGroupForm(project, group));
      else if (chat) add('Настройки чата', () => openChatForm(chat, project), busy);
      else add('Настройки проекта', () => openProjectForm(project));
    }
    if (!project.archived || (!chat && !group)) {
      add(archived ? 'Восстановить' : 'Архивировать', () => archive(project, chat, group, !archived), !archived && busy);
    }
    if (project.archived || archived) {
      add('Удалить навсегда', () => deleteArchived(project, chat, group), busy, true);
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

  function updateContextControls() {
    const doc = contextEditor?.doc;
    const dirty = doc && contextBrief.value !== contextEditor.baseBrief;
    const editable = doc?.editable && !contextEditor.readonly;
    contextBrief.readOnly = !editable || saving || !online;
    get('group-context-save').disabled = !available() || !editable || !dirty || contextEditor.stale;
    get('group-context-reload').disabled = !online || saving;
    get('group-context-download').disabled = !doc;
    get('group-context-close').disabled = saving;
  }

  function applyContextDocument(doc, force = false) {
    if (!contextEditor) return;
    const dirty = contextEditor.doc && contextBrief.value !== contextEditor.baseBrief;
    const oldRevision = contextEditor.baseRevision;
    const oldMarkdown = contextEditor.doc?.markdown;
    contextEditor.doc = doc;
    if (force || !dirty) {
      contextBrief.value = doc.brief;
      contextEditor.baseBrief = doc.brief;
      contextEditor.baseRevision = doc.brief_revision;
      contextEditor.stale = false;
    } else if (oldRevision !== doc.brief_revision) {
      contextEditor.stale = true;
      report('Общая цель изменена в другом окне. Скопируйте черновик и обновите контекст.', true);
    }
    get('group-context-title').textContent = doc.name + ' · CONTEXT.md';
    if (oldMarkdown !== doc.markdown) {
      get('group-context-preview').replaceChildren(renderMarkdown(doc.markdown));
    }
    updateContextControls();
  }

  async function refreshGroupContext(force = false) {
    if (!contextDialog.open || !contextEditor || !online) return;
    const editor = contextEditor;
    const requestId = ++contextRequest;
    try {
      const doc = await request(`/projects/${editor.projectId}/groups/${editor.groupId}/context`);
      if (requestId === contextRequest && contextEditor === editor && contextDialog.open) {
        if (force) report();
        applyContextDocument(doc, force);
      }
    } catch (error) {
      if (requestId === contextRequest && contextEditor === editor && contextDialog.open) {
        report(error.message, true);
      }
    }
  }

  function openGroupContext(project, group) {
    if (!available()) return;
    contextEditor = { projectId: project.id, groupId: group.id, readonly: archiveView || project.archived };
    contextBrief.value = '';
    get('group-context-title').textContent = group.name + ' · CONTEXT.md';
    get('group-context-preview').replaceChildren(element('p', 'Загружаем контекст…'));
    contextDialog.showModal();
    report();
    updateContextControls();
    refreshGroupContext(true);
  }

  contextBrief.oninput = updateContextControls;
  contextDialog.oncancel = event => {
    if (saving || (contextEditor?.doc && contextBrief.value !== contextEditor.baseBrief &&
        !window.confirm('Закрыть без сохранения общей цели?'))) event.preventDefault();
  };
  contextDialog.onclose = () => { contextRequest++; contextEditor = undefined; };
  get('group-context-close').onclick = () => {
    if (saving) return;
    if (contextEditor?.doc && contextBrief.value !== contextEditor.baseBrief &&
        !window.confirm('Закрыть без сохранения общей цели?')) return;
    contextDialog.close();
  };
  get('group-context-reload').onclick = () => {
    if (contextEditor?.doc && contextBrief.value !== contextEditor.baseBrief &&
        !window.confirm('Обновить контекст и сбросить несохранённые изменения?')) return;
    refreshGroupContext(true);
  };
  get('group-context-download').onclick = () => {
    if (!contextEditor?.doc) return;
    const url = URL.createObjectURL(new Blob([contextEditor.doc.markdown], { type: 'text/markdown;charset=utf-8' }));
    const link = document.createElement('a');
    link.href = url;
    link.download = 'CONTEXT.md';
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };
  contextForm.onsubmit = event => {
    event.preventDefault();
    if (get('group-context-save').disabled || !contextEditor?.doc) return;
    const editor = contextEditor;
    const brief = contextBrief.value;
    ++contextRequest;
    save('/projects/groups/context', {
      project_id: editor.projectId, group_id: editor.groupId,
      brief, brief_revision: editor.baseRevision,
    }, async doc => {
      if (contextEditor !== editor) return;
      applyContextDocument(doc, true);
      report('Общая цель сохранена.');
    });
  };

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
      archiveView ? project.archived || visibleChats(project).length : !project.archived)
      .sort((a, b) => b.id - a.id);
    const previousChat = chatId;
    const project = projects.find(project => project.id === (preferred?.project ?? projectId)) ?? projects[0];
    const chats = project ? visibleChats(project) : [];
    const requested = preferred ? preferred.chat : project?.id === projectId ? chatId : chats[0]?.id;
    projectId = project?.id;
    chatId = chats.find(chat => chat.id === requested)?.id;
    onSelection({ projectId, chatId });
    const chat = selectedChat();
    if (chat && preferred?.chat === chat.id) {
      collapsedProjects.delete(projectId);
      if (chat.group) collapsedGroups.delete(`${projectId}/group/${chat.group.id}`);
    }
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
    workspace.textContent = !chat ? '' : !primary ? 'Внутренняя рабочая папка проекта' :
      [primary, ...(workingFolders.length > 1 ? [`+${workingFolders.length - 1}`] : [])].join(' · ');
    workspace.title = workingFolders.join('\n');
    const notice = get('project-archive-notice');
    notice.hidden = !project?.archived && !chat?.archived;
    notice.textContent = project?.archived ? 'Проект в архиве. Восстанови проект, чтобы продолжить работу.'
      : chat?.archived ? 'Чат в архиве. Восстанови чат, чтобы продолжить переписку.' : '';
    renderHistory();
    updateControls();
    return refreshHistory();
  }

  function detail(key, className, summary) {
    const node = element('details', '');
    node.className = className;
    node.dataset.detailKey = key;
    node.append(summary);
    node.open = expandedDetails.has(key);
    return node;
  }

  function thinkingDetail(message, key) {
    const summary = element('summary', message.model ? `Размышления · ${message.model}` : 'Размышления модели');
    const node = detail(key, 'project-thinking', summary);
    node.append(element('p', message.thinking));
    return node;
  }

  function toolDetail(message) {
    const tool = message.tool;
    const summary = element('summary', '');
    const label = element('span', '');
    label.className = 'project-tool-label';
    const name = element('span', tool.name);
    name.className = 'project-tool-name';
    label.append(name);
    if (tool.target) {
      const target = element('span', tool.target);
      target.className = 'project-tool-target';
      target.title = tool.target;
      label.append(target);
    }
    const statuses = {
      pending: ['…', 'Ожидает результата'], ok: ['✓', 'Готово'],
      error: ['×', 'Ошибка'], cancelled: ['–', 'Остановлено'], unknown: ['?', 'Неизвестно'],
    };
    const [mark, status] = statuses[tool.status] ?? statuses.unknown;
    const indicator = element('span', mark);
    indicator.className = 'project-tool-indicator';
    indicator.title = status;
    indicator.setAttribute('aria-label', status);
    indicator.setAttribute('role', 'img');
    summary.append(label, indicator);
    const node = detail('tool:' + tool.id, 'project-tool', summary);
    node.dataset.status = tool.status;
    // Do not create large argument/result DOM nodes until this action is opened.
    const populate = () => {
      if (!node.open || node.querySelector('.project-tool-body')) return;
      const body = element('div', '');
      body.className = 'project-tool-body';
      appendMessageTime(body, message);
      body.append(element('h4', 'Аргументы'), element('pre', tool.arguments || '{}'));
      if (tool.result != null) {
        body.append(element('h4', 'Результат'), element('pre', tool.result || '(пусто)'));
      }
      node.append(body);
    };
    node.addEventListener('toggle', populate);
    populate();
    return node;
  }

  function renderActivity(block, index, active) {
    const item = element('li', '');
    item.dataset.role = 'activity';
    const entries = block.entries;
    const tools = entries.filter(entry => entry.tool);
    const skills = entries.filter(entry => entry.role === 'notice');
    const count = tools.length + skills.length;
    const first = entries[0];
    const key = 'activity:' + (first.tool?.id ?? `${first.timestamp ?? 'unknown'}:${index}`);
    if (!count) {
      for (const [i, message] of entries.entries()) {
        item.append(thinkingDetail(message, key + ':thinking:' + i));
      }
      return item;
    }
    const summary = element('summary', '');
    const name = element('span', `Действия · ${count}`);
    name.className = 'project-activity-label';
    summary.append(name);
    const pending = tools.filter(entry => entry.tool.status === 'pending').length;
    const errors = tools.filter(entry => entry.tool.status === 'error').length;
    if (pending || errors) {
      const status = element('span', [pending && `В работе: ${pending}`, errors && `Ошибки: ${errors}`].filter(Boolean).join(' · '));
      status.className = 'project-activity-status';
      if (errors) status.dataset.error = 'true';
      summary.append(status);
    }
    const node = detail(key, 'project-activity', summary);
    node.open = activityOpen(active, activityOverrides.get(key));
    node.dataset.initialOpen = String(node.open);
    const rows = element('div', '');
    rows.className = 'project-activity-rows';
    for (const [i, message] of entries.entries()) {
      if (message.tool) rows.append(toolDetail(message));
      else if (message.thinking) rows.append(thinkingDetail(message, key + ':thinking:' + i));
      else {
        const skill = element('p', message.text);
        skill.className = 'project-activity-skill';
        rows.append(skill);
      }
    }
    node.append(rows);
    item.append(node);
    return item;
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
    // Snapshot open states before replacing history. Keep the outer and inner
    // disclosures independent even while tool results stream in.
    expandedDetails.clear();
    if (renderedChatId === chatId) {
      for (const node of messages.querySelectorAll('details[data-detail-key]')) {
        const key = node.dataset.detailKey;
        if (node.classList.contains('project-activity')) {
          // Only a user change becomes an override; an automatic open
          // must not keep a finished batch expanded on the next render.
          if (String(node.open) !== node.dataset.initialOpen) activityOverrides.set(key, node.open);
        } else if (node.open) expandedDetails.add(key);
      }
    } else {
      activityOverrides.clear();
    }
    renderedChatId = chatId;
    const blocks = groupChatHistory(entries);
    const working = Boolean(chatRun(chatId));
    messages.replaceChildren(...blocks.map((block, index) => {
      if (block.kind === 'activity') return renderActivity(block, index, working && index === blocks.length - 1);
      const message = block.message;
      const item = element('li', '');
      item.dataset.role = message.role;
      item.dataset.origin = message.origin ?? '';
      appendMessageTime(item, message);
      const label = message.role === 'assistant' ? (message.model || 'Lyra')
        : { orchestrator: 'Оркестратор', lyra: 'Lyra · Следующий шаг' }[message.origin] ?? '';
      if (label) item.append(element('strong', label));
      if (message.thinking) {
        item.append(thinkingDetail(message, `message:${message.timestamp ?? 'unknown'}:${index}`));
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
    const label = task?.model ? `Размышления · ${task.model}` : 'Размышления модели';
    const summary = get('project-live-thinking-label');
    if (summary.textContent !== label) summary.textContent = label;
    if (thinkingDelegation !== id) {
      thinkingDelegation = id;
      liveThinking.open = false;
    }
    liveThinking.hidden = !text.trim();
    if (liveThinkingText.textContent === text) return;
    const reading = messages.parentElement;
    const atEnd = reading.scrollHeight - reading.scrollTop - reading.clientHeight < 24;
    liveThinkingText.textContent = text;
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
      if (contextDialog.open && !saving) refreshGroupContext();
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

  function deleteArchived(project, chat, group) {
    closeEntryMenu();
    const target = chat ? `чат «${chat.name}»` : group ? `группу «${group.name}»` : `проект «${project.name}»`;
    const scope = !chat && !group ? 'Все его чаты и история будут удалены.' :
      'История и связанные архивные чаты задач будут удалены.';
    if (!window.confirm(`Удалить навсегда ${target}?\n\n${scope} Рабочие файлы и папки останутся на диске. Отменить действие нельзя.`)) return;
    save('/projects/archive/delete', {
      project_id: project.id,
      project_session_id: chat?.id,
      group_id: group?.id,
    }, async result => {
      for (const id of result.deleted_session_ids) {
        drafts.delete(id);
        chatModels.delete(id);
      }
      await refresh();
      report(result.history_cleanup_failed
        ? 'Удалено из архива, но часть файлов истории не удалось очистить. Подробности в журнале Lyra.'
        : 'Удалено навсегда.', Boolean(result.history_cleanup_failed));
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
    if (!delegation && ['queued', 'context_full', 'output_limit'].includes(chat?.job?.status)) {
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
    sending.set(chat.id, { role: 'user', origin: 'dashboard', model, text, request_id: requestId, timestamp: Date.now() });
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
      restoreDraft = reply.structuredContent?.stop_reason === 'cancelled_by_user';
      if (chatId === chat.id) {
        if (restoreDraft) report('Остановлено пользователем.');
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
    async openCommandChat(owner) {
      const preferred = { project: owner.project_id, chat: owner.chat_id };
      let project = catalog?.projects.find(project => project.id === preferred.project);
      let chat = project?.sessions.find(chat => chat.id === preferred.chat);
      if (!chat) {
        await refresh();
        project = catalog?.projects.find(project => project.id === preferred.project);
        chat = project?.sessions.find(chat => chat.id === preferred.chat);
      }
      if (!chat) return false;
      const archived = Boolean(project.archived || chat.archived);
      archiveView = archived;
      get('show-active-projects').setAttribute('aria-pressed', String(!archived));
      get('show-archived-projects').setAttribute('aria-pressed', String(archived));
      preferredSelection = undefined;
      await select(preferred);
      return projectId === preferred.project && chatId === preferred.chat;
    },
    update(connected, state, models) {
      const previous = chatRun(chatId);
      online = connected;
      run = state;
      installedModels = models ?? [];
      updateControls();
      const current = chatRun(chatId);
      if (Boolean(previous) !== Boolean(current) || (current && current[0] !== previous?.[0])) {
        renderHistory();
      }
      if ((previous && !current && connected) || (current && (
        current[0] !== previous?.[0] ||
        current[1].history_revision !== previous?.[1].history_revision
      ))) {
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
