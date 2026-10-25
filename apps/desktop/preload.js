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
  parseLink: (text) => call('link:parse', text),
  pair: (address, code) => call('pair', address, code),
  pollPrompts: () => call('prompts:poll'),
  answerPrompt: (id, accept) => call('prompts:answer', id, accept),
  connect: (address) => call('connect', address),
  discover: (seconds) => call('discover', seconds),
  connectQuick: (fingerprint) => call('connect:quick', fingerprint),
  disconnect: () => call('disconnect'),
  pollConnection: () => call('connection:poll'),
  requestVideo: (offer) => call('media:offer', offer),
  requestDisplayVideo: (display, offer) => call('media:display-offer', display, offer),
  openViewers: () => call('viewer:open-all'),
  sendInput: (event) => ipcRenderer.send('input:send', event),
  sendClipboard: () => call('clipboard:send'),
  getClipboard: () => call('clipboard:get'),
  listDisplays: () => call('displays:list'),
  wake: (target) => call('wake:send', target),
  startHost: (options) => call('host:start', options),
  stopHost: () => call('host:stop'),
  pollHost: () => call('host:poll'),
});
