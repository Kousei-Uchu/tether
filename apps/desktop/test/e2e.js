// Drives the real window: pairs, connects, sees live permission changes and a revoke.
// Run with: npm run e2e   (on Linux without a display: xvfb-run -a npm run e2e)
const { app, BrowserWindow } = require('electron');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const clientHome = fs.mkdtempSync(path.join(os.tmpdir(), 'tether-e2e-client-'));
const hostHome = fs.mkdtempSync(path.join(os.tmpdir(), 'tether-e2e-host-'));
process.env.TETHER_HOME = clientHome;
require('../main.js');

const { Device } = require('../../../crates/tether-node');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function fail(message) {
  console.log(`E2E FAILED: ${message}`);
  app.exit(1);
}

async function waitFor(win, expr, what, timeoutMs = 8000) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    if (await win.webContents.executeJavaScript(expr)) return;
    await sleep(100);
  }
  throw new Error(`timed out waiting for ${what}`);
}

async function run() {
  await app.whenReady();
  let win;
  while (!(win = BrowserWindow.getAllWindows()[0])) await sleep(50);
  await new Promise((r) => (win.webContents.isLoading() ? win.webContents.once('did-finish-load', r) : r()));
  const js = (code) => win.webContents.executeJavaScript(code);

  const hostDev = new Device(hostHome, 'host-pc');
  const handle = await hostDev.listen('127.0.0.1:0', { grant: ['view', 'input'] }, 'test-pattern', true);

  // Pair through the UI.
  await js(`document.getElementById('address').value = ${JSON.stringify(handle.address)}`);
  await js(`document.getElementById('code').value = ${JSON.stringify(handle.pairingCode)}`);
  await js(`document.getElementById('pair-btn').click()`);
  await waitFor(win, `document.getElementById('connect-msg').textContent.includes('Paired with host-pc')`, 'pairing message');

  // The paired-devices table shows the host with no permissions (client side entry).
  await waitFor(win, `document.querySelectorAll('#peers tbody tr').length === 1`, 'peers table row');

  // Connect and read the grant.
  await js(`document.getElementById('connect-btn').click()`);
  await waitFor(win, `!document.getElementById('connection').hidden`, 'connection card');
  if ((await js(`document.getElementById('conn-name').textContent`)) !== 'host-pc') throw new Error('wrong peer name');
  const on = () => js(`document.querySelectorAll('#conn-perms .chip.on').length`);
  if ((await on()) !== 2) throw new Error(`expected 2 permissions, saw ${await on()}`);

  // The host grants media; the UI picks it up on its next poll (every 3 seconds).
  hostDev.grant(hostDev.peers()[0].fingerprint, ['media']);
  await waitFor(win, `document.querySelectorAll('#conn-perms .chip.on').length === 3`, 'live grant', 6000);

  // Watch the test video. The host draws a 16 bit frame counter along the bottom edge; read it
  // back out of the decoded picture twice to prove frames really are arriving and advancing.
  await js(`document.getElementById('show-screen').click()`);
  await waitFor(
    win,
    `(() => { const v = document.getElementById('screen'); return !v.hidden && v.videoWidth > 0 && v.readyState >= 2; })()`,
    'video to start playing',
    20000,
  );
  const readCounter = () =>
    js(`(() => {
      const v = document.getElementById('screen');
      const c = document.createElement('canvas');
      c.width = v.videoWidth; c.height = v.videoHeight;
      const ctx = c.getContext('2d');
      ctx.drawImage(v, 0, 0);
      const block = c.width / 16, y = c.height - 16;
      let n = 0;
      for (let bit = 0; bit < 16; bit++) {
        const d = ctx.getImageData(Math.floor(bit * block + block / 2), y, 1, 1).data;
        n = n * 2 + ((d[0] + d[1] + d[2]) / 3 > 127 ? 1 : 0);
      }
      return { n, w: c.width, h: c.height };
    })()`);
  const first = await readCounter();
  if (first.w !== 960 || first.h !== 540) throw new Error(`unexpected video size ${first.w}x${first.h}`);
  await sleep(1500);
  const second = await readCounter();
  if (!(second.n > first.n)) throw new Error(`video is not advancing: ${first.n} then ${second.n}`);
  console.log(`video ok: frame counter ${first.n} then ${second.n}`);

  // Control the host. The host here injects into the very X display this window lives on, so
  // the events come back as real (trusted) browser events and prove the whole chain.
  await js(`(() => {
    window.__seen = { moves: [], keys: [] };
    document.addEventListener('mousemove', (e) => e.isTrusted && window.__seen.moves.push([e.screenX, e.screenY]), true);
    document.addEventListener('keydown', (e) => e.isTrusted && window.__seen.keys.push(e.code), true);
    document.getElementById('code').focus();   // keep DOM focus off the video so injected keys do not echo
    const box = document.getElementById('control');
    box.checked = true;
    box.dispatchEvent(new Event('change'));
  })()`);
  win.focus();
  await js(`document.getElementById('code').focus()`);
  const { width: sw, height: sh } = require('electron').screen.getPrimaryDisplay().size;
  const screenSize = { w: sw, h: sh };

  // A synthetic mouse move over the middle of the picture, near its top edge.
  const sendAt = (nx, ny) => js(`(() => {
    const v = document.getElementById('screen');
    const b = v.getBoundingClientRect();
    const s = Math.min(b.width / v.videoWidth, b.height / v.videoHeight);
    const cw = v.videoWidth * s, ch = v.videoHeight * s;
    const left = b.left + (b.width - cw) / 2, top = b.top + (b.height - ch) / 2;
    v.dispatchEvent(new MouseEvent('mousemove', { clientX: left + ${nx} * cw, clientY: top + ${ny} * ch, bubbles: true }));
  })()`);
  // Aim at the header strip of this window, which is not part of the video, so the pointer
  // arrives as a real event here without feeding back into the viewer.
  const bounds = win.getBounds();
  const want = [bounds.x + 400, bounds.y + 20];
  await sendAt(want[0] / screenSize.w, want[1] / screenSize.h);
  await waitFor(
    win,
    `window.__seen.moves.some(([x, y]) => Math.abs(x - ${want[0]}) <= 3 && Math.abs(y - ${want[1]}) <= 3)`,
    `the pointer to reach ${want.map(Math.round)} on this screen`,
    8000,
  );
  console.log(`input ok: pointer arrived near ${want.map(Math.round)}`);

  // A key press and release.
  await js(`(() => {
    const v = document.getElementById('screen');
    v.dispatchEvent(new KeyboardEvent('keydown', { code: 'KeyQ', key: 'q', bubbles: true }));
    v.dispatchEvent(new KeyboardEvent('keyup', { code: 'KeyQ', key: 'q', bubbles: true }));
  })()`);
  await waitFor(win, `window.__seen.keys.includes('KeyQ')`, 'the key press to arrive', 8000);
  console.log('input ok: key arrived');

  // Taking the input permission away disables the control box in the window.
  hostDev.revoke(hostDev.peers()[0].fingerprint, ['input']);
  await waitFor(win, `document.getElementById('control').disabled && !document.getElementById('control').checked`, 'control to switch off after revoke', 8000);
  hostDev.grant(hostDev.peers()[0].fingerprint, ['input']);
  await waitFor(win, `!document.getElementById('control').disabled`, 'control to be allowed again', 8000);

  // Taking the view permission away stops the viewer in the window.
  hostDev.revoke(hostDev.peers()[0].fingerprint, ['view']);
  await waitFor(win, `document.getElementById('show-screen').disabled && document.getElementById('screen').hidden`, 'viewer to stop after revoke', 8000);
  hostDev.grant(hostDev.peers()[0].fingerprint, ['view']);
  await waitFor(win, `!document.getElementById('show-screen').disabled`, 'viewing to be allowed again', 8000);

  // Host from the UI too.
  await js(`document.getElementById('bind').value = '127.0.0.1:0'`);
  await js(`document.getElementById('host-start').click()`);
  await waitFor(win, `!document.getElementById('host-running').hidden`, 'host running');
  const code = await js(`document.getElementById('host-code').textContent`);
  if (!/^[0-9A-Z]{4}(-[0-9A-Z]{4}){3}$/.test(code)) throw new Error(`bad pairing code ${code}`);
  await js(`document.getElementById('host-stop').click()`);
  await waitFor(win, `document.getElementById('host-running').hidden`, 'host stopped');

  // Unpairing on the host ends the session and the UI says why.
  hostDev.remove(hostDev.peers()[0].fingerprint);
  await waitFor(win, `document.getElementById('connection').hidden`, 'connection closed', 6000);
  const msg = await js(`document.getElementById('connect-msg').textContent`);
  if (!msg.includes('access revoked')) throw new Error(`expected revoke message, saw: ${msg}`);

  handle.stop();
  console.log('E2E OK');
  app.exit(0);
}

run().catch((e) => fail(e.message));
