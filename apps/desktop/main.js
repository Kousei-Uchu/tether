// Electron main process. Owns the Rust bridge; the renderer never touches it directly.
const { app, BrowserWindow, ipcMain, clipboard } = require('electron');
const path = require('node:path');
const os = require('node:os');
const QRCode = require('qrcode');
const fs = require('node:fs');
// Packaged builds carry the bridge in ./native (see scripts/stage-native.mjs); a source
// checkout loads it straight from the workspace.
const bridgePath = fs.existsSync(path.join(__dirname, 'native', 'tether.node'))
  ? './native'
  : '../../crates/tether-node';
const { Device, pairingLink, parsePairingLink, listDisplays } = require(bridgePath);

const smoke = process.argv.includes('--smoke');
const PERMISSIONS = ['view', 'input', 'media', 'power', 'files'];

// What the host may stream: nothing, the test pattern, or a display ("screen" or "screen:<id>").
function videoSource(value) {
  if (!value) return null;
  if (value === true) return 'test-pattern';
  const text = String(value);
  if (!/^(test-pattern|screen(:\d+)?)$/.test(text)) throw new Error('Unknown video source.');
  return text;
}

// Chromium hides local addresses behind mDNS names by default. Tether peers are your own
// machines on a LAN or Tailscale, and the host cannot resolve those names, so turn it off.
app.commandLine.appendSwitch('disable-features', 'WebRtcHideLocalIpsWithMdns');

let device = null;
let win = null;
let connection = null;

// One window per screen of the computer we are connected to, so each can sit on its own
// monitor. They share the one connection and close with it.
const viewers = new Set();

function closeViewers() {
  for (const win of [...viewers]) if (!win.isDestroyed()) win.close();
  viewers.clear();
}

function openViewer(display, index, hostName) {
  const width = 960;
  const height = Math.max(240, Math.min(900, Math.round((width * display.height) / display.width) + 44));
  const win = new BrowserWindow({
    width,
    height,
    x: 60 + index * 40,
    y: 60 + index * 40,
    title: `${hostName}: ${display.name}`,
    backgroundColor: '#000000',
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
  win.loadFile(path.join(__dirname, 'renderer', 'viewer.html'), {
    query: {
      display: String(display.id),
      name: display.name,
      host: hostName,
      audio: index === 0 ? '1' : '0',
    },
  });
  viewers.add(win);
  win.on('closed', () => viewers.delete(win));
  return win;
}
let host = null;
let hostLog = [];
let hostLinks = [];

// Mouse and keyboard events go to the host in order. Pointer moves are replaceable, so only
// the latest waiting one is kept; everything else keeps its place in line.
const INPUT_KINDS = new Set(['move', 'delta', 'button', 'scroll', 'key', 'text']);
const inputQueue = [];
let pendingMove = null;
let inputBusy = false;

function cleanInput(raw) {
  if (!raw || typeof raw !== 'object' || !INPUT_KINDS.has(raw.kind)) return null;
  const out = { kind: raw.kind };
  for (const key of ['x', 'y', 'dx', 'dy', 'display']) if (typeof raw[key] === 'number') out[key] = raw[key];
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

  handle('link:parse', (text) => parsePairingLink(String(text)));
  handle('pair', (address, code) => device.pair(String(address), String(code)));

  handle('connect', async (address) => {
    closeViewers();
    if (connection) await connection.close().catch(() => {});
    connection = await device.connect(String(address));
    return connectionState();
  });
  handle('disconnect', async () => {
    closeViewers();
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
      closeViewers();
      return { connected: false, lostReason: e instanceof Error ? e.message : String(e) };
    }
  });

  ipcMain.on('input:send', (_event, raw) => queueInput(raw));

  handle('media:offer', (offer) => {
    if (!connection) throw new Error('Not connected.');
    return connection.requestVideo(String(offer));
  });

  handle('media:display-offer', (display, offer) => {
    if (!connection) throw new Error('Not connected.');
    if (!Number.isInteger(display)) throw new Error('Unknown display.');
    return connection.requestDisplayVideo(display, String(offer));
  });

  handle('viewer:open-all', async () => {
    if (!connection) throw new Error('Not connected.');
    const list = await connection.listRemoteDisplays();
    const hostName = connection.peer?.name || 'Host';
    closeViewers();
    list.forEach((display, i) => openViewer(display, i, hostName));
    return list.length;
  });

  handle('displays:list', () => {
    try {
      return listDisplays();
    } catch {
      return []; // no screen to share, for example a machine with no display attached
    }
  });
  // The text itself never goes through the page: main reads and writes this computer's
  // clipboard and the page only sees what the other computer said about it.
  handle('clipboard:send', async () => {
    if (!connection) throw new Error('Not connected.');
    // Electron's clipboard calls return promises from version 44, and older ones return the
    // value itself; await works for both.
    const text = await clipboard.readText();
    if (!text) throw new Error('There is no text on this computer\'s clipboard.');
    return connection.sendClipboard(text);
  });
  handle('clipboard:get', async () => {
    if (!connection) throw new Error('Not connected.');
    const reply = await connection.getClipboard();
    if (!reply.text) return { copied: false, message: reply.notice || 'The other computer has no text on its clipboard.' };
    await clipboard.writeText(reply.text);
    return { copied: true, message: 'Copied the other computer\'s clipboard to this one.' };
  });

  handle('host:start', async ({ bind, pairing, grant, video, audio, input, clipboard: shareClipboard }) => {
    if (host) throw new Error('Already accepting connections.');
    const options = pairing ? { grant: validPermissions(grant ?? ['view']) } : null;
    host = await device.listen(
      String(bind),
      options,
      videoSource(video),
      Boolean(input),
      Boolean(shareClipboard),
      video && audio ? 'system' : null,
    );
    hostLog = [];
    hostLinks = await buildLinks();
    return hostState();
  });
  handle('host:stop', () => {
    if (host) host.stop();
    host = null;
    hostLinks = [];
    logHost('info', 'Stopped accepting connections');
    return hostState();
  });
  handle('host:poll', () => {
    if (host) for (const e of host.drainEvents()) logHost(e.kind, e.message);
    return hostState();
  });
}

// Addresses another device could use to reach this one, most likely first: private LAN
// ranges, then Tailscale, then anything else.
function reachableAddresses(port) {
  const rank = (ip) => (/^(192\.168|10\.|172\.(1[6-9]|2\d|3[01]))/.test(ip) ? 0 : ip.startsWith('100.') ? 1 : 2);
  const ips = Object.values(os.networkInterfaces())
    .flat()
    .filter((i) => i && (i.family === 'IPv4' || i.family === 4) && !i.internal)
    .map((i) => i.address)
    .sort((a, b) => rank(a) - rank(b));
  return ips.map((ip) => `${ip}:${port}`);
}

// One QR code per address this host can be reached at. Scanning one pairs a phone without
// typing the address or the code.
async function buildLinks() {
  const [bindHost, port] = [host.address.slice(0, host.address.lastIndexOf(':')), host.address.split(':').pop()];
  const wildcard = bindHost === '0.0.0.0' || bindHost === '[::]' || bindHost === '::';
  let addresses = wildcard ? reachableAddresses(port) : [host.address];
  if (addresses.length === 0) addresses = [host.address];
  const code = host.pairingCode;
  if (!code) return [];
  return Promise.all(
    addresses.map(async (address) => {
      const link = pairingLink(address, code, device.name);
      const svg = await QRCode.toString(link, { type: 'svg', margin: 1, width: 240, errorCorrectionLevel: 'M' });
      return { address, link, svgDataUrl: `data:image/svg+xml;base64,${Buffer.from(svg).toString('base64')}` };
    }),
  );
}

function hostState() {
  return {
    running: host !== null,
    address: host ? host.address : null,
    code: host ? host.pairingCode : null,
    links: hostLinks,
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
