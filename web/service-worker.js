self.addEventListener('notificationclick', event => {
  event.notification.close();
  event.waitUntil(openDashboard());
});

async function openDashboard() {
  const windows = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
  const dashboard = windows.find(client => new URL(client.url).pathname === '/');
  if (dashboard) {
    await dashboard.focus();
    dashboard.postMessage('show-command');
  } else {
    await self.clients.openWindow('/#command-panel');
  }
}
