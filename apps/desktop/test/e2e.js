// Drives the real window: pairs, connects, sees live permission changes and a revoke.
// Run with: npm run e2e   (on Linux without a display: xvfb-run -a npm run e2e)
// No person is here to compare symbols, so pairing runs on the code alone.
process.env.TETHER_SKIP_VERIFY = '1';
const { app, BrowserWindow } = require('electron');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const clientHome = fs.mkdtempSync(path.join(os.tmpdir(), 'tether-e2e-client-'));
const hostHome = fs.mkdtempSync(path.join(os.tmpdir(), 'tether-e2e-host-'));
app.setPath('userData', fs.mkdtempSync(path.join(os.tmpdir(), 'tether-e2e-profile-')));
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
  // A socket standing in for the sleeping computer's network card.
  const udp = require('node:dgram').createSocket('udp4');
  const wakePacket = new Promise((resolve) => udp.once('message', resolve));
  await new Promise((resolve) => udp.bind(0, '127.0.0.1', resolve));
  const handle = await hostDev.listen(
    '127.0.0.1:0',
    { grant: ['view', 'input'] },
    'test-pattern',
    true,
    true,
    'tone',
    [`desk=aa:bb:cc:dd:ee:ff@127.0.0.1:${udp.address().port}`],
  );

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

  // The host also sends a test tone. Chromium counts the Opus packets it receives and
  // decodes; there is no sound card here, so count rather than listen.
  const audioStats = async () =>
    win.webContents.executeJavaScript(`(async () => {
      const out = { packets: 0, samples: 0, energy: 0 };
      for (const r of (await viewer.getStats()).values()) {
        if (r.type === 'inbound-rtp' && r.kind === 'audio') {
          out.packets = r.packetsReceived || 0;
          out.samples = r.totalSamplesReceived || 0;
          out.energy = r.totalAudioEnergy || 0;
        }
      }
      return out;
    })()`);
  let a1 = await audioStats();
  await sleep(1500);
  let a2 = await audioStats();
  if (!(a2.packets > a1.packets && a2.packets > 20)) {
    throw new Error(`audio is not arriving: ${JSON.stringify(a1)} then ${JSON.stringify(a2)}`);
  }
  console.log(`audio ok: ${a1.packets} then ${a2.packets} packets, decoded samples ${a2.samples}, energy ${a2.energy.toFixed(3)}`);

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

  // The media row presses real media keys on the host.
  await js(`document.querySelector('#media-row button[data-key="AudioVolumeUp"]').click()`);
  await waitFor(win, `window.__seen.keys.includes('AudioVolumeUp')`, 'the volume key to arrive', 8000);
  console.log('input ok: media key arrived');

  // Every screen of the host in its own window, each with its own video.
  await js(`document.getElementById('open-viewers').click()`);
  let views = [];
  for (let i = 0; i < 100 && views.length < 2; i++) {
    views = BrowserWindow.getAllWindows().filter((w) => w !== win);
    await sleep(100);
  }
  if (views.length !== 2) throw new Error(`expected 2 screen windows, saw ${views.length}`);
  const sizes = {};
  for (const v of views) {
    await waitFor(v, `document.getElementById('screen').videoWidth > 0`, 'a screen window to show video', 15000);
    const size = await v.webContents.executeJavaScript(
      `document.getElementById('screen').videoWidth + 'x' + document.getElementById('screen').videoHeight`,
    );
    sizes[v.getTitle()] = size;
  }
  const bySize = Object.values(sizes).sort().join(',');
  if (bySize !== '480x270,640x360') throw new Error(`unexpected screen sizes ${JSON.stringify(sizes)}`);
  console.log(`screens ok: ${JSON.stringify(sizes)}`);

  // Aiming at the middle of the second screen moves the host pointer to the middle of that
  // screen's part of the desktop (it starts 640 pixels to the right).
  const secondView = views.find((v) => sizes[v.getTitle()] === '480x270');
  await secondView.webContents.executeJavaScript(`(() => {
    const box = document.getElementById('control');
    box.checked = true;
    box.dispatchEvent(new Event('change'));
  })()`);
  await secondView.webContents.executeJavaScript(`(() => {
    const v = document.getElementById('screen');
    const b = v.getBoundingClientRect();
    const s = Math.min(b.width / v.videoWidth, b.height / v.videoHeight);
    const cw = v.videoWidth * s, ch = v.videoHeight * s;
    const left = b.left + (b.width - cw) / 2, top = b.top + (b.height - ch) / 2;
    v.dispatchEvent(new MouseEvent('mousemove', { clientX: left + 0.5 * cw, clientY: top + 0.5 * ch, bubbles: true }));
  })()`);
  const wantX = 640 + Math.round(0.5 * 479);
  const wantY = Math.round(0.5 * 269);
  let at = { x: -1, y: -1 };
  for (let i = 0; i < 80; i++) {
    at = require('electron').screen.getCursorScreenPoint();
    if (Math.abs(at.x - wantX) <= 3 && Math.abs(at.y - wantY) <= 3) break;
    await sleep(100);
  }
  if (Math.abs(at.x - wantX) > 3 || Math.abs(at.y - wantY) > 3) {
    throw new Error(`pointer at ${at.x},${at.y}, wanted near ${wantX},${wantY}`);
  }
  console.log(`screens ok: pointer on screen 2 arrived near ${wantX},${wantY}`);
  for (const v of views) v.close();

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

  // Clipboard buttons: hidden without `files`, shown once granted, and both work. The host
  // and this window share one clipboard here, so what goes over comes straight back; the
  // bridge tests cover that the text itself really crosses (it comes from an argument there).
  const { clipboard } = require('electron');
  const fp = hostDev.peers()[0].fingerprint;
  if (!(await js(`document.getElementById('clipboard-box').hidden`))) throw new Error('clipboard shown without files');
  hostDev.grant(fp, ['files']);
  await waitFor(win, `!document.getElementById('clipboard-box').hidden`, 'clipboard buttons after grant', 8000);
  const text = 'e2e clipboard ✓\nsecond line';
  await clipboard.writeText(text);
  await js(`document.getElementById('clip-send').click()`);
  await waitFor(win, `document.getElementById('clip-msg').textContent.includes("Copied to the host's clipboard")`, 'send to host', 8000);
  await js(`document.getElementById('clip-get').click()`);
  await waitFor(win, `document.getElementById('clip-msg').textContent.includes('Copied the other computer')`, 'get from host', 8000);
  const back = await clipboard.readText();
  if (back !== text) throw new Error(`clipboard text changed on the way: ${JSON.stringify(back)}`);
  console.log('clipboard ok: send and get from the window');
  // An empty local clipboard is explained rather than sent.
  await clipboard.clear();
  await js(`document.getElementById('clip-send').click()`);
  await waitFor(win, `document.getElementById('clip-msg').classList.contains('error')`, 'empty clipboard message', 8000);
  hostDev.revoke(fp, ['files']);
  await waitFor(win, `document.getElementById('clipboard-box').hidden`, 'clipboard buttons to hide after revoke', 8000);

  // Wake button: hidden without `power`, shown once granted, sends a real magic packet,
  // and a name the relay does not know is explained.
  if (!(await js(`document.getElementById('wake-row').hidden`))) throw new Error('wake row shown without power');
  hostDev.grant(fp, ['power']);
  await waitFor(win, `!document.getElementById('wake-row').hidden`, 'wake row after the power grant', 8000);
  await js(`document.getElementById('wake-target').value = 'desk'`);
  await js(`document.getElementById('wake-send').click()`);
  await waitFor(win, `document.getElementById('wake-msg').textContent.includes('Sent a wake signal to desk')`, 'wake answer', 8000);
  const packet = await Promise.race([wakePacket, sleep(5000).then(() => null)]);
  const mac = Buffer.from('aabbccddeeff', 'hex');
  if (!packet || packet.length !== 102 || !packet.subarray(0, 6).equals(Buffer.alloc(6, 255)) || !packet.subarray(96).equals(mac)) {
    throw new Error('the magic packet did not arrive intact');
  }
  console.log('wake ok: magic packet arrived');
  await sleep(3300); // the host allows one wake every three seconds
  await js(`document.getElementById('wake-target').value = 'laptop'`);
  await js(`document.getElementById('wake-send').click()`);
  await waitFor(win, `document.getElementById('wake-msg').textContent.includes('does not know')`, 'unknown name answer', 8000);
  hostDev.revoke(fp, ['power']);
  await waitFor(win, `document.getElementById('wake-row').hidden`, 'wake row to hide after revoke', 8000);
  udp.close();

  // Host from the UI too.
  await js(`document.getElementById('bind').value = '127.0.0.1:0'`);
  await js(`document.getElementById('host-start').click()`);
  await waitFor(win, `!document.getElementById('host-running').hidden`, 'host running');
  const code = await js(`document.getElementById('host-code').textContent`);
  if (!/^[0-9A-Z]{4}(-[0-9A-Z]{4}){3}$/.test(code)) throw new Error(`bad pairing code ${code}`);
  // The QR code on screen decodes to the same link that is shown as text, and that link
  // reads back to this host's address and code.
  const shown = await js(`({ link: document.getElementById('host-link').textContent, src: document.getElementById('host-qr').src })`);
  const pixels = await js(`new Promise((resolve, reject) => {
    const img = document.getElementById('host-qr');
    const draw = () => {
      const c = document.createElement('canvas');
      c.width = 360; c.height = 360;
      const ctx = c.getContext('2d');
      ctx.fillStyle = '#fff'; ctx.fillRect(0, 0, 360, 360);
      ctx.drawImage(img, 0, 0, 360, 360);
      resolve(Array.from(ctx.getImageData(0, 0, 360, 360).data));
    };
    img.complete && img.naturalWidth ? draw() : (img.onload = draw, img.onerror = () => reject(new Error('QR image failed to load')));
  })`);
  const decoded = require('jsqr')(Uint8ClampedArray.from(pixels), 360, 360);
  if (!decoded) throw new Error('the QR code could not be decoded');
  if (decoded.data !== shown.link) throw new Error(`QR says ${decoded.data} but the page shows ${shown.link}`);
  const parsed = await js(`window.tether.parseLink(${JSON.stringify(shown.link)})`);
  if (parsed.code !== code) throw new Error(`link code ${parsed.code} is not ${code}`);
  console.log(`qr ok: ${shown.link}`);
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
