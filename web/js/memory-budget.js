import { memoryAmount } from './ui.js';

function memoryRoom(memory, resident) {
  // The live sample already includes this model's resident memory.
  const other = Math.max(0, memory.used - (resident ?? 0));
  return Math.max(0, memory.total - other);
}

export function residentRamBytes(model) {
  return Number.isFinite(model?.size) && Number.isFinite(model.size_vram) && model.size_vram >= 0
    ? Math.max(0, model.size - model.size_vram) : 0;
}

export function contextMemoryWarning(snapshot, model, resident, contextBytes) {
  const weights = snapshot?.model_sizes?.[model];
  if (!Number.isFinite(weights) || weights <= 0 || snapshot.models == null) return '';
  const estimate = weights + (contextBytes ?? 0);
  const compute = Math.max(512 * 2 ** 20, Math.ceil(estimate * 0.1));
  const systemReserve = 2 ** 30;
  const needs = estimate + compute;
  const scope = contextBytes == null ? ' · без оценки контекста' : '';
  const gpus = snapshot.gpus.filter((gpu) => gpu.vram?.total > 0);
  const ram = snapshot.ram;
  const ramAvailable = ram?.total > 0 ? memoryRoom(ram, residentRamBytes(resident)) : null;

  if (gpus.length === 0) {
    if (ramAvailable == null || needs + systemReserve <= ramAvailable) return '';
    return `При загрузке в RAM может не хватить ≈ ${memoryAmount(needs + systemReserve - ramAvailable)}${scope}. Учтены веса модели, запас для системы и ≈ ${memoryAmount(compute)} на вычисления.`;
  }

  // Ollama reports per-model VRAM across devices, so reclaim it only for one measured GPU.
  const gpuAvailable = Math.max(...gpus.map((gpu) =>
    memoryRoom(gpu.vram, gpus.length === 1 ? resident?.size_vram : 0)));
  const shortfall = Math.max(0, needs - gpuAvailable);
  const ramShortfall = ramAvailable == null ? 0 : Math.max(0, shortfall + systemReserve - ramAvailable);
  if (shortfall > 0) {
    const gpuLabel = gpus.length > 1 ? 'На самой свободной GPU' : 'В VRAM';
    const fallback = ramShortfall > 0 ?
      ` При переносе остатка в RAM там тоже может не хватить ≈ ${memoryAmount(ramShortfall)}.` :
      ' Ollama может перенести часть нагрузки в RAM.';
    return `${gpuLabel} может не хватить ≈ ${memoryAmount(shortfall)}${scope}. Учтён запас ≈ ${memoryAmount(compute)} на вычисления.${fallback}`;
  }
  return ramShortfall > 0 ? 'Мало свободной RAM. Под фоновые процессы и систему заложен запас 1 ГБ.' : '';
}
