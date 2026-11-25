export function postModel(path, selected, payload, target) {
  const options = { method: 'POST', headers: { 'X-Lyra-Model': selected ?? '' } };
  if (target && target !== selected) options.headers['X-Lyra-Target-Model'] = target;
  if (payload) {
    options.headers['Content-Type'] = 'application/json';
    options.body = JSON.stringify(payload);
  }
  return fetch(path, options);
}

export function modelResponseError(response) {
  if (response.status === 409) return 'Модель изменилась или выполняется другая операция.';
  if (response.status === 404) return 'Модель больше не установлена.';
  if (response.status === 503) return 'Ollama недоступна.';
  return `Запрос завершился ошибкой (${response.status}).`;
}

export function sameModelName(left, right) {
  return left === right || left === `${right}:latest` || `${left}:latest` === right;
}
