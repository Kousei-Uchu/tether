// Electron main process. Owns the Rust bridge; the renderer never touches it directly.
const { app, BrowserWindow, ipcMain } = require('electron');
const path = require('node:path');
const { Device } = require('../../crates/tether-node');

const smoke = process.argv.includes('--smoke');
const PERMISSIONS = ['view', 'input', 'media', 'power', 'files'];

let device = null;
let win = null;
let connection = null;
let host = null;
let hostLog = [];

function logHost(kind, message) {
  hostLog.push({ kind, message, at: Date.now() });
  if (hostLog.length > 100) hostLog = hostLog.slice(-100);
}

// Every handler answers { ok, value } or { ok, error } so the renderer sees plain messages
// instead of Electron's wrapped IPC errors.
function handle(channel, fn) {
  ipcMain.handle(channel, async (_event, ...args) => {
    try {
      return { ok: true, value: await fn(...args) };
    } catch (e) {
      return { ok: false, error: e instanceof Error ? e.message : String(e) };
    }
  });
}

function validPermissions(list) {
  if (!Array.isArray(list) || !list.every((p) => PERMISSIONS.includes(p))) {
    throw new Error('Unknown permission.');
  }
  return list;
}

function connectionState() {
  if (!connection) return { connected: false };
  return { connected: true, peer: connection.peer, permissions: connection.permissions };
}

function registerIpc() {
  handle('device:info', () => ({
    name: device.name,
    fingerprint: device.fingerprint,
    permissions: PERMISSIONS,
  }));

  handle('peers:list', () => device.peers());
  handle('peers:grant', (query, perms) => device.grant(String(query), validPermissions(perms)));
  handle('peers:revoke', (query, perms) => device.revoke(String(query), validPermissions(perms)));
  handle('peers:remove', (query) => device.remove(String(query)));

  handle('pair', (address, code) => device.pair(String(address), String(code)));

  handle('connect', async (address) => {
    if (connection) await connection.close().catch(() => {});
    connection = await device.connect(String(address));
    return connectionState();
  });
  handle('disconnect', async () => {
    if (connection) await connection.close().catch(() => {});
    connection = null;
    return connectionState();
  });
  handle('connection:poll', async () => {
    if (!connection) return { connected: false };
    try {
      const rttMs = await connection.ping();
      return { ...connectionState(), rttMs };
    } catch (e) {
      connection = null;
      return { connected: false, lostReason: e instanceof Error ? e.message : String(e) };
    }
  });

  handle('host:start', async ({ bind, pairing, grant }) => {
    if (host) throw new Error('Already accepting connections.');
    const options = pairing ? { grant: validPermissions(grant ?? ['view']) } : null;
    host = await device.listen(String(bind), options);
    hostLog = [];
    return hostState();
  });
  handle('host:stop', () => {
    if (host) host.stop();
    host = null;
    logHost('info', 'Stopped accepting connections');
    return hostState();
  });
  handle('host:poll', () => {
    if (host) for (const e of host.drainEvents()) logHost(e.kind, e.message);
    return hostState();
  });
}

function hostState() {
  return {
    running: host !== null,
    address: host ? host.address : null,
    code: host ? host.pairingCode : null,
    log: hostLog,
  };
}

function createWindow() {
  win = new BrowserWindow({
    width: 980,
    height: 760,
    minWidth: 720,
    minHeight: 560,
    title: 'Tether',
    show: !smoke,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });
  win.setMenuBarVisibility(false);
  win.webContents.on('will-navigate', (e) => e.preventDefault());
  win.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
  win.loadFile(path.join(__dirname, 'renderer', 'index.html'));

  if (smoke) {
    win.webContents.once('did-finish-load', async () => {
      await new Promise((r) => setTimeout(r, 800));
      const shown = await win.webContents.executeJavaScript(
        "document.getElementById('device-name').textContent",
      );
      const fp = await win.webContents.executeJavaScript(
        "document.getElementById('device-fingerprint').textContent",
      );
      const ok = shown && shown !== '...' && /^([0-9a-f]{4}-){7}[0-9a-f]{4}$/.test(fp);
      console.log(ok ? `SMOKE OK ${shown} ${fp}` : `SMOKE FAILED name=${shown} fp=${fp}`);
      app.exit(ok ? 0 : 1);
    });
  }
}

app.whenReady().then(() => {
  device = new Device(process.env.TETHER_HOME || null, null);
  registerIpc();
  createWindow();
  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on('before-quit', () => {
  if (host) host.stop();
  if (connection) connection.close().catch(() => {});
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit();
});
