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
  return bytes >= 2 ** 30 ? `${oneDecimal.format(bytes / 2 ** 30)} ГиБ` : `${oneDecimal.format(bytes / 2 ** 20)} МиБ`;
}

export function memoryLabel(value) {
  if (!value) return '—';
  const inGigabytes = value.total >= 2 ** 30;
  const unit = inGigabytes ? 2 ** 30 : 2 ** 20;
  const format = inGigabytes ? oneDecimal : whole;
  return `${format.format(value.used / unit)} / ${format.format(value.total / unit)} ${inGigabytes ? 'ГиБ' : 'МиБ'}`;
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
