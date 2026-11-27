import { memoryAmount } from './ui.js';

export function residentRamBytes(model) {
  return Number.isFinite(model?.size) && Number.isFinite(model.size_vram) && model.size_vram >= 0
    ? Math.max(0, model.size - model.size_vram) : 0;
}

export function contextMemoryWarning(memory) {
  const vram = memory?.vram_shortfall_bytes;
  const ram = memory?.ram_shortfall_bytes;
  if (vram > 0) {
    const scope = memory.kv_included ? '' : ' без учёта KV-кэша';
    const fallback = ram > 0 ? ` В RAM может не хватить ещё ≈ ${memoryAmount(ram)}.` : '';
    return `В бюджете VRAM не хватает ≈ ${memoryAmount(vram)}${scope}. Ollama может перенести часть нагрузки в RAM.${fallback}`;
  }
  return ram > 0 ? `В RAM может не хватить ≈ ${memoryAmount(ram)} с учётом запаса для системы.` : '';
}
