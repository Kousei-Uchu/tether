// Takes screenshots of the real window for design review: npm run shots (under xvfb on Linux).
const { app, BrowserWindow } = require('electron');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const clientHome = fs.mkdtempSync(path.join(os.tmpdir(), 'tether-shot-client-'));
const hostHome = fs.mkdtempSync(path.join(os.tmpdir(), 'tether-shot-host-'));
app.setPath('userData', fs.mkdtempSync(path.join(os.tmpdir(), 'tether-e2e-profile-')));
process.env.TETHER_HOME = clientHome;
require('../main.js');
const { Device } = require('../../../crates/tether-node');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const out = process.env.SHOTS_DIR || '/tmp/shots';
const tag = process.env.SHOTS_TAG || 'now';

async function run() {
  await app.whenReady();
  let win;
  while (!(win = BrowserWindow.getAllWindows()[0])) await sleep(50);
  await new Promise((r) => (win.webContents.isLoading() ? win.webContents.once('did-finish-load', r) : r()));
  const js = (code) => win.webContents.executeJavaScript(code);
  const shot = async (name) => {
    await sleep(600);
    const img = await win.webContents.capturePage();
    fs.writeFileSync(path.join(out, `${tag}-${name}.png`), img.toPNG());
    console.log('shot', name, img.getSize().width + 'x' + img.getSize().height);
  };
  await shot('1-start');
  const hostDev = new Device(hostHome, 'Studio Mac');
  const handle = await hostDev.listen('127.0.0.1:0', { grant: ['view', 'input', 'media', 'files'] }, 'test-pattern', true, true, 'tone', []);
  await js(`document.getElementById('address').value = ${JSON.stringify(handle.address)}`);
  await js(`document.getElementById('code').value = ${JSON.stringify(handle.pairingCode)}`);
  await js(`document.getElementById('pair-btn').click()`);
  await sleep(1500);
  await shot('2-paired');
  await js(`document.getElementById('connect-btn').click()`);
  await sleep(2000);
  await js(`document.getElementById('show-screen').click()`);
  await sleep(3000);
  await shot('3-connected');
  await js(`document.querySelector('[data-view=host]').click()`);
  await shot('4-host-settings');
  await js(`document.getElementById('host-start').click()`);
  await sleep(1500);
  await shot('5-hosting');
  await js(`document.getElementById('disconnect-btn').click()`);
  await sleep(800);
  await js(`document.querySelector('[data-view=computers]').click()`);
  await shot('6-computers');
  app.exit(0);
}
run().catch((e) => { console.log('shots failed', e); app.exit(1); });
