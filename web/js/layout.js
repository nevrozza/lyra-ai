import { get } from './ui.js';

function enableColumnResize(handle, grid, pane, property, unit, layout) {
  const styleOverrides = layout.dashboardLayout.style;
  const previousKey = `lyra.layout.${pane.id}.width`;
  const storageKey = unit === 'cqw' ? `lyra.layout.${pane.id}.dashboard-width` : previousKey;
  let previousPercent;
  let drag;
  try {
    const savedWidth = Number(localStorage.getItem(storageKey));
    if (savedWidth > 0 && savedWidth <= 100) {
      styleOverrides.setProperty(property, `${savedWidth}${unit}`);
    } else if (storageKey !== previousKey) {
      previousPercent = Number(localStorage.getItem(previousKey));
    }
  } catch {
    // The layout is still adjustable when browser storage is disabled.
  }

  function limits() {
    const style = getComputedStyle(grid);
    const gap = parseFloat(style.columnGap);
    const separateBenchmark = grid === layout.dashboardLayout && layout.benchmarkResizer.offsetWidth;
    const benchmarkSpace = separateBenchmark ? layout.benchmarkPanel.getBoundingClientRect().width + gap : 0;
    const min = parseFloat(style.getPropertyValue('--column-min')) + benchmarkSpace;
    const max = grid.getBoundingClientRect().width - parseFloat(style.getPropertyValue('--neighbor-min')) - gap;
    return { min, max, referenceWidth: layout.dashboardLayout.getBoundingClientRect().width };
  }

  function resize(size, { min, max, referenceWidth }) {
    // Both separators use the same reference width, even though the benchmark is nested.
    const percent = 100 * Math.max(min, Math.min(size, max)) / referenceWidth;
    styleOverrides.setProperty(property, `${percent}${unit}`);
  }

  function save() {
    try {
      localStorage.setItem(storageKey, parseFloat(styleOverrides.getPropertyValue(property)));
    } catch {
      // Keep the current width for this session.
    }
  }

  handle.addEventListener('pointerdown', (event) => {
    if (event.button !== 0 || !event.isPrimary) return;
    handle.focus({ preventScroll: true });
    handle.setPointerCapture(event.pointerId);
    drag = { id: event.pointerId, start: event.clientX, size: pane.getBoundingClientRect().width,
      range: limits(), previous: styleOverrides.getPropertyValue(property) };
    event.preventDefault();
  });
  handle.addEventListener('pointermove', (event) => {
    if (drag?.id === event.pointerId) resize(drag.size + drag.start - event.clientX, drag.range);
  });
  handle.addEventListener('pointerup', (event) => {
    if (drag?.id !== event.pointerId) return;
    if (event.clientX !== drag.start || styleOverrides.getPropertyValue(property) !== drag.previous) {
      resize(drag.size + drag.start - event.clientX, drag.range);
      save();
    }
    drag = undefined;
  });
  handle.addEventListener('lostpointercapture', (event) => {
    if (drag?.id !== event.pointerId) return;
    styleOverrides.setProperty(property, drag.previous);
    drag = undefined;
  });
  handle.addEventListener('keydown', (event) => {
    if (drag || !['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    const range = limits();
    const size = pane.getBoundingClientRect().width;
    const target = {
      ArrowLeft: size + 24,
      ArrowRight: size - 24,
      Home: range.min,
      End: range.max,
    }[event.key];
    resize(target, range);
    save();
  });

  const observer = new ResizeObserver(() => {
    if (!handle.offsetWidth) return;
    const range = limits();
    if (previousPercent > 0 && previousPercent <= 100) {
      // Older clients saved the benchmark as a share of the model workspace.
      resize(grid.getBoundingClientRect().width * previousPercent / 100, range);
      previousPercent = undefined;
      save();
    }
    const width = Math.round(pane.getBoundingClientRect().width);
    handle.setAttribute('aria-valuemin', Math.round(range.min));
    handle.setAttribute('aria-valuemax', Math.round(range.max));
    handle.setAttribute('aria-valuenow', width);
  });
  observer.observe(grid);
  observer.observe(pane);
  if (grid === layout.dashboardLayout) observer.observe(layout.benchmarkPanel);
}

export function initLayout() {
  const dashboardLayout = get('dashboard-layout');
  const modelWorkspace = get('model-workspace');
  const benchmarkPanel = get('benchmark-panel');
  const benchmarkResizer = get('benchmark-resizer');
  const layout = { dashboardLayout, benchmarkPanel, benchmarkResizer };
  enableColumnResize(get('workspace-resizer'), dashboardLayout, modelWorkspace, '--workspace-width', '%', layout);
  enableColumnResize(benchmarkResizer, modelWorkspace, benchmarkPanel, '--benchmark-width', 'cqw', layout);
}
