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

 
// The modal backdrop retargets pointer events to the dialog. Its own padding
// also targets the dialog, so test the actual bounds instead of target alone.
export function isOutsideDialog(event, dialog) {
  if (event.target !== dialog) return false;
  const rect = dialog.getBoundingClientRect();
  return event.clientX < rect.left || event.clientX > rect.right ||
    event.clientY < rect.top || event.clientY > rect.bottom;
}

export function initDialogDismissal(root = document) {
  // Auto popovers already implement native light dismissal. Modern dialogs
  // use closedby="any"; older engines get the same cancelable behavior.
  for (const dialog of root.querySelectorAll('dialog[closedby="any"]')) {
    if ('closedBy' in dialog) continue;
    let outsidePointer;
    let releasedOutside = false;
    dialog.addEventListener('pointerdown', event => {
      outsidePointer = dialog.open && event.isPrimary && event.button === 0 &&
        isOutsideDialog(event, dialog) ? event.pointerId : undefined;
      releasedOutside = false;
    });
    dialog.addEventListener('pointerup', event => {
      releasedOutside = outsidePointer === event.pointerId && isOutsideDialog(event, dialog);
      outsidePointer = undefined;
    });
    dialog.addEventListener('pointercancel', () => {
      outsidePointer = undefined;
      releasedOutside = false;
    });
    dialog.addEventListener('close', () => {
      outsidePointer = undefined;
      releasedOutside = false;
    });
    dialog.addEventListener('click', event => {
      const dismiss = dialog.open && releasedOutside && isOutsideDialog(event, dialog);
      releasedOutside = false;
      if (!dismiss) return;
      if (typeof dialog.requestClose === 'function') dialog.requestClose();
      else if (dialog.dispatchEvent(new Event('cancel', { cancelable: true }))) dialog.close();
    });
  }
}


// A small tolerance avoids switching modes because of fractional pixels.
export function nearChatEnd(scroller, threshold = 48) {
  return scroller.scrollHeight - scroller.clientHeight - scroller.scrollTop <= threshold;
}

export function chatFollowTarget(scroller, anchorTop = Infinity) {
  const end = Math.max(0, scroller.scrollHeight - scroller.clientHeight);
  return Math.max(0, Math.min(end, anchorTop));
}

export function createChatScroll(scroller, bottomButton) {
  let following = true;
  let lastApplied;
  let anchor;
  let anchorTimer;
  let frame;
  const update = () => {
    scroller.dataset.follow = String(following);
    bottomButton.hidden = following || nearChatEnd(scroller);
  };
  const clearAnchor = () => {
    clearTimeout(anchorTimer);
    anchor = undefined;
  };
  function followNow() {
    if (!following) { update(); return; }
    if (anchor && !anchor.isConnected) clearAnchor();
    const limit = anchor ? scroller.scrollTop + anchor.getBoundingClientRect().top -
      scroller.getBoundingClientRect().top - 12 : Infinity;
    const end = chatFollowTarget(scroller);
    // Do not move backwards to expose a summary already above the viewport.
    const target = chatFollowTarget(scroller, Math.max(scroller.scrollTop, limit));
    if (target < end - 1) {
      following = false;
      clearAnchor();
    }
    if (Math.abs(scroller.scrollTop - target) > 1) {
      scroller.scrollTop = target;
      lastApplied = scroller.scrollTop;
    }
    update();
  }
  function changed() {
    if (frame != null) return;
    frame = requestAnimationFrame(() => {
      frame = undefined;
      followNow();
    });
  }
  function goToEnd() {
    clearAnchor();
    following = true;
    followNow();
  }
  scroller.addEventListener('scroll', () => {
    // Ignore our own movement; user scrolling up must suspend following.
    if (lastApplied != null && Math.abs(scroller.scrollTop - lastApplied) <= 1) {
      lastApplied = undefined;
      update();
      return;
    }
    lastApplied = undefined;
    following = nearChatEnd(scroller);
    if (!following) clearAnchor();
    update();
  }, { passive: true });
  scroller.addEventListener('click', event => {
    const summary = event.target.closest?.('summary');
    const details = summary?.parentElement;
    if (!details || details.tagName !== 'DETAILS' || details.open || !following) return;
    // For a manually expanded block, follow its growth only while the
    // heading remains visible. Large results should be read from the top.
    clearAnchor();
    anchor = summary;
    anchorTimer = setTimeout(clearAnchor, 350);
    changed();
  });
  bottomButton.addEventListener('click', goToEnd);
  const observer = new ResizeObserver(changed);
  observer.observe(scroller);
  for (const child of scroller.children) observer.observe(child);
  update();
  return { changed, goToEnd };
}

export function iconButton(label, path) {
  const button = element('button', '');
  button.type = 'button';
  button.title = label;
  button.setAttribute('aria-label', label);
  // Static icon paths are authored locally, never from server configuration.
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  for (const [key, value] of Object.entries({ width: 16, height: 16, viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', 'stroke-width': 1.7, 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'aria-hidden': 'true' })) svg.setAttribute(key, value);
  const shape = document.createElementNS(svg.namespaceURI, 'path');
  shape.setAttribute('d', path);
  svg.append(shape);
  button.append(svg);
  return button;
}

export const trashIcon = 'M3 6h18M8 6V3h8v3m3 0-1 15H6L5 6m5 4v7m4-7v7';

export function settingsSwitch(name, enabled) {
  const toggle = element('button', '');
  toggle.type = 'button';
  toggle.className = 'settings-switch';
  toggle.setAttribute('role', 'switch');
  toggle.setAttribute('aria-checked', String(enabled));
  toggle.setAttribute('aria-label', name + ': включён');
  toggle.title = enabled ? 'Выключить' : 'Включить';
  return toggle;
}


// Reconcile settings rows by name. Keep the real switch and action buttons
// mounted so confirmed state changes animate and focus survives a refresh.
export function reconcileSettingsRows(list, rows) {
  const previous = new Map([...list.children].map(row => [row.dataset.setting, row]));
  const initial = list.dataset.rendered !== 'true';
  const next = rows.map(row => {
    const old = previous.get(row.dataset.setting);
    if (!old || old.lastElementChild.children.length !== row.lastElementChild.children.length) {
      if (!initial && !old) row.classList.add('settings-row-enter');
      return row;
    }
    const info = row.firstElementChild;
    if (!old.firstElementChild.isEqualNode(info)) old.firstElementChild.replaceWith(info);
    if (row.dataset.enabled != null) old.dataset.enabled = row.dataset.enabled;
    const controls = [...old.lastElementChild.children];
    for (const [index, fresh] of [...row.lastElementChild.children].entries()) {
      const control = controls[index];
      for (const attribute of ['aria-checked', 'aria-label', 'title']) {
        const value = fresh.getAttribute(attribute);
        if (value == null) control.removeAttribute(attribute);
        else if (control.getAttribute(attribute) !== value) control.setAttribute(attribute, value);
      }
      control.onclick = fresh.onclick;
    }
    return old;
  });
  const retained = new Set(next);
  for (const child of [...list.children]) if (!retained.has(child)) child.remove();
  for (const [index, row] of next.entries()) {
    if (list.children[index] !== row) list.insertBefore(row, list.children[index] ?? null);
  }
  list.dataset.rendered = 'true';
}
