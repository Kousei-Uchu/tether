// Exposes a small, fixed API to the page. The page gets no Node access.
const { contextBridge, ipcRenderer } = require('electron');

async function call(channel, ...args) {
  const result = await ipcRenderer.invoke(channel, ...args);
  if (!result.ok) throw new Error(result.error);
  return result.value;
}

contextBridge.exposeInMainWorld('tether', {
  deviceInfo: () => call('device:info'),
  listPeers: () => call('peers:list'),
  grant: (query, perms) => call('peers:grant', query, perms),
  revoke: (query, perms) => call('peers:revoke', query, perms),
  removePeer: (query) => call('peers:remove', query),
  pair: (address, code) => call('pair', address, code),
  connect: (address) => call('connect', address),
  disconnect: () => call('disconnect'),
  pollConnection: () => call('connection:poll'),
  requestVideo: (offer) => call('media:offer', offer),
  startHost: (options) => call('host:start', options),
  stopHost: () => call('host:stop'),
  pollHost: () => call('host:poll'),
});
