// Electron main process. Owns the Rust bridge; the renderer never touches it directly.
const { app, BrowserWindow, ipcMain } = require('electron');
const path = require('node:path');
const { Device } = require('../../crates/tether-node');

const smoke = process.argv.includes('--smoke');
const PERMISSIONS = ['view', 'input', 'media', 'power', 'files'];

// Chromium hides local addresses behind mDNS names by default. Tether peers are your own
// machines on a LAN or Tailscale, and the host cannot resolve those names, so turn it off.
app.commandLine.appendSwitch('disable-features', 'WebRtcHideLocalIpsWithMdns');

let device = null;
let win = null;
let connection = null;
let host = null;
let hostLog = [];

// Mouse and keyboard events go to the host in order. Pointer moves are replaceable, so only
// the latest waiting one is kept; everything else keeps its place in line.
const INPUT_KINDS = new Set(['move', 'delta', 'button', 'scroll', 'key', 'text']);
const inputQueue = [];
let pendingMove = null;
let inputBusy = false;

function cleanInput(raw) {
  if (!raw || typeof raw !== 'object' || !INPUT_KINDS.has(raw.kind)) return null;
  const out = { kind: raw.kind };
  for (const key of ['x', 'y', 'dx', 'dy']) if (typeof raw[key] === 'number') out[key] = raw[key];
  for (const key of ['button', 'code', 'text']) if (typeof raw[key] === 'string') out[key] = raw[key];
  if (typeof raw.down === 'boolean') out.down = raw.down;
  return out;
}

function queueInput(raw) {
  const event = cleanInput(raw);
  if (!event || !connection) return;
  if (event.kind === 'move') {
    pendingMove = event;
  } else {
    if (pendingMove) {
      inputQueue.push(pendingMove);
      pendingMove = null;
    }
    if (inputQueue.length < 500) inputQueue.push(event);
  }
  pumpInput();
}

async function pumpInput() {
  if (inputBusy) return;
  inputBusy = true;
  try {
    while (connection && (inputQueue.length > 0 || pendingMove)) {
      let event = inputQueue.shift();
      if (!event) {
        event = pendingMove;
        pendingMove = null;
      }
      await connection.sendInput(event).catch(() => {});
    }
  } finally {
    inputBusy = false;
  }
}

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
      const notices = await connection.takeNotices();
      return { ...connectionState(), rttMs, notices };
    } catch (e) {
      connection = null;
      return { connected: false, lostReason: e instanceof Error ? e.message : String(e) };
    }
  });

  ipcMain.on('input:send', (_event, raw) => queueInput(raw));

  handle('media:offer', (offer) => {
    if (!connection) throw new Error('Not connected.');
    return connection.requestVideo(String(offer));
  });

  handle('host:start', async ({ bind, pairing, grant, video, input }) => {
    if (host) throw new Error('Already accepting connections.');
    const options = pairing ? { grant: validPermissions(grant ?? ['view']) } : null;
    host = await device.listen(String(bind), options, video ? 'test-pattern' : null, Boolean(input));
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
