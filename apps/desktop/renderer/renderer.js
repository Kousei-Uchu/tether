// All text from the other side (device names, platforms) is shown with textContent only.
const $ = (id) => document.getElementById(id);
const PERMS = ['view', 'input', 'media', 'power', 'files'];
const PERM_LABEL = {
  view: 'View screen and audio',
  input: 'Mouse and keyboard',
  media: 'Media and volume',
  power: 'Power and wake',
  files: 'Clipboard and files',
};

function el(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  Object.assign(node, props);
  for (const c of children) node.append(c);
  return node;
}

function say(id, text, isError = false) {
  const node = $(id);
  node.textContent = text;
  node.classList.toggle('error', isError);
}

async function guarded(msgId, fn) {
  try {
    say(msgId, '');
    return await fn();
  } catch (e) {
    say(msgId, e.message, true);
    return undefined;
  }
}

// ----- this device -----
async function loadIdentity() {
  const info = await window.tether.deviceInfo();
  $('device-name').textContent = info.name;
  $('device-fingerprint').textContent = info.fingerprint;
}

// ----- watching the screen -----
let viewer = null;
let mayViewNow = false;
let mayControlNow = false;

function waitForIce(pc, ms) {
  return new Promise((resolve) => {
    if (pc.iceGatheringState === 'complete') return resolve();
    const done = () => {
      pc.removeEventListener('icegatheringstatechange', check);
      resolve();
    };
    const check = () => pc.iceGatheringState === 'complete' && done();
    pc.addEventListener('icegatheringstatechange', check);
    setTimeout(done, ms);
  });
}

function stopVideo(message) {
  if (viewer) {
    viewer.onconnectionstatechange = null;
    viewer.close();
    viewer = null;
  }
  setControl(false);
  const video = $('screen');
  video.srcObject = null;
  video.hidden = true;
  $('control-row').hidden = true;
  $('stop-screen').hidden = true;
  $('show-screen').disabled = !mayViewNow;
  if (message !== undefined) say('screen-msg', message);
}

async function startVideo() {
  stopVideo();
  say('screen-msg', 'Asking for the screen...');
  $('show-screen').disabled = true;
  const pc = new RTCPeerConnection({ iceServers: [] });
  viewer = pc;
  try {
    const transceiver = pc.addTransceiver('video', { direction: 'recvonly' });
    // Hosts send H.264 only, so only offer that.
    const h264 = RTCRtpReceiver.getCapabilities('video').codecs.filter((c) => c.mimeType === 'video/H264');
    if (h264.length === 0) throw new Error('This computer cannot play H.264 video.');
    transceiver.setCodecPreferences(h264);

    pc.ontrack = (event) => {
      const video = $('screen');
      video.srcObject = event.streams[0] ?? new MediaStream([event.track]);
      video.hidden = false;
      $('control-row').hidden = false;
      $('stop-screen').hidden = false;
      say('screen-msg', '');
    };
    pc.onconnectionstatechange = () => {
      if (viewer !== pc) return;
      if (['failed', 'closed', 'disconnected'].includes(pc.connectionState)) {
        stopVideo('The video stopped.');
      }
    };

    await pc.setLocalDescription(await pc.createOffer());
    await waitForIce(pc, 3000);
    const answer = await window.tether.requestVideo(pc.localDescription.sdp);
    if (viewer !== pc) return;
    await pc.setRemoteDescription({ type: 'answer', sdp: answer });
  } catch (e) {
    if (viewer === pc) stopVideo(e.message);
  }
}

// ----- controlling the host -----
// Events from this page go to the host, which presses the real keys. Pointer positions are
// sent as a fraction of the picture, so the two screens can differ in size and shape.
const heldKeys = new Set();
const heldButtons = new Set();
let controlOn = false;
let moveFrame = null;
let lastPoint = null;
const BUTTONS = ['left', 'middle', 'right', 'back', 'forward'];

function send(event) {
  window.tether.sendInput(event);
}

function pictureRect() {
  const video = $('screen');
  const box = video.getBoundingClientRect();
  if (!video.videoWidth || !video.videoHeight) return null;
  // The picture is letterboxed inside the element (object-fit: contain).
  const scale = Math.min(box.width / video.videoWidth, box.height / video.videoHeight);
  const width = video.videoWidth * scale;
  const height = video.videoHeight * scale;
  return { left: box.left + (box.width - width) / 2, top: box.top + (box.height - height) / 2, width, height };
}

function toFraction(event) {
  const r = pictureRect();
  if (!r) return null;
  const x = (event.clientX - r.left) / r.width;
  const y = (event.clientY - r.top) / r.height;
  if (x < 0 || x > 1 || y < 0 || y > 1) return null;
  return { x, y };
}

function releaseAll() {
  for (const code of heldKeys) send({ kind: 'key', code, down: false });
  for (const button of heldButtons) send({ kind: 'button', button, down: false });
  heldKeys.clear();
  heldButtons.clear();
}

function setControl(on) {
  controlOn = on;
  $('control').checked = on;
  if (on) $('screen').focus();
  else releaseAll();
}

function scheduleMove(point) {
  lastPoint = point;
  if (moveFrame !== null) return;
  moveFrame = requestAnimationFrame(() => {
    moveFrame = null;
    if (controlOn && lastPoint) send({ kind: 'move', ...lastPoint });
  });
}

$('control').addEventListener('change', (e) => setControl(e.target.checked && mayControlNow));

const screen = $('screen');
screen.addEventListener('mousemove', (e) => {
  const p = controlOn && toFraction(e);
  if (p) scheduleMove(p);
});
screen.addEventListener('mousedown', (e) => {
  const p = controlOn && toFraction(e);
  if (!p) return;
  e.preventDefault();
  screen.focus();
  const button = BUTTONS[e.button];
  if (!button) return;
  send({ kind: 'move', ...p });
  send({ kind: 'button', button, down: true });
  heldButtons.add(button);
});
window.addEventListener('mouseup', (e) => {
  const button = BUTTONS[e.button];
  if (controlOn && button && heldButtons.delete(button)) send({ kind: 'button', button, down: false });
});
screen.addEventListener('contextmenu', (e) => controlOn && e.preventDefault());
screen.addEventListener(
  'wheel',
  (e) => {
    if (!controlOn) return;
    e.preventDefault();
    const toLines = (d) => (d === 0 ? 0 : Math.sign(d) * (e.deltaMode === 1 ? Math.abs(d) : Math.max(1, Math.round(Math.abs(d) / 40))));
    send({ kind: 'scroll', dx: toLines(e.deltaX), dy: toLines(e.deltaY) });
  },
  { passive: false },
);
screen.addEventListener('keydown', (e) => {
  if (!controlOn) return;
  e.preventDefault();
  heldKeys.add(e.code);
  send({ kind: 'key', code: e.code, down: true });
});
screen.addEventListener('keyup', (e) => {
  if (!controlOn) return;
  e.preventDefault();
  heldKeys.delete(e.code);
  send({ kind: 'key', code: e.code, down: false });
});
// Taking the keyboard back must not leave a key held down on the other computer.
screen.addEventListener('blur', () => releaseAll());
window.addEventListener('blur', () => releaseAll());

$('show-screen').addEventListener('click', startVideo);
$('stop-screen').addEventListener('click', () => stopVideo(''));

// ----- connecting -----
let pollTimer = null;

function showConnection(state) {
  const box = $('connection');
  box.hidden = !state.connected;
  mayViewNow = state.connected && state.permissions.includes('view');
  mayControlNow = state.connected && state.permissions.includes('input');
  $('control').disabled = !mayControlNow;
  if (!mayControlNow && $('control').checked) setControl(false);
  if (!state.connected) {
    stopVideo();
    return;
  }
  const mayView = mayViewNow;
  $('show-screen').disabled = !mayView || viewer !== null;
  if (!mayView && viewer) stopVideo('The host turned off your access to its screen.');
  if (!mayView && !viewer && !$('screen-msg').textContent) {
    say('screen-msg', 'This device is not allowed to view the screen. Ask the host to grant it.');
  } else if (mayView && $('screen-msg').textContent.startsWith('This device is not allowed')) {
    say('screen-msg', '');
  }
  if (state.notices && state.notices.length > 0) {
    say('screen-msg', `Host says: ${state.notices[state.notices.length - 1]}`);
  }
  $('conn-name').textContent = state.peer.name;
  $('conn-platform').textContent = state.peer.platform;
  $('conn-fingerprint').textContent = state.peer.fingerprint;
  $('conn-rtt').textContent = state.rttMs !== undefined ? `${state.rttMs.toFixed(1)} ms` : '';
  const list = $('conn-perms');
  list.replaceChildren(
    ...PERMS.map((p) =>
      el('li', { className: state.permissions.includes(p) ? 'chip on' : 'chip', textContent: PERM_LABEL[p] }),
    ),
  );
}

function startPolling() {
  stopPolling();
  pollTimer = setInterval(async () => {
    try {
      const state = await window.tether.pollConnection();
      showConnection(state);
      if (!state.connected) {
        stopPolling();
        if (state.lostReason) say('connect-msg', `Disconnected: ${state.lostReason}`, true);
      }
    } catch (e) {
      stopPolling();
      say('connect-msg', e.message, true);
    }
  }, 3000);
}

function stopPolling() {
  if (pollTimer) clearInterval(pollTimer);
  pollTimer = null;
}

$('pair-btn').addEventListener('click', () =>
  guarded('connect-msg', async () => {
    const address = $('address').value.trim();
    const code = $('code').value.trim();
    if (!address || !code) throw new Error('Enter the address and the pairing code.');
    const peer = await window.tether.pair(address, code);
    $('code').value = '';
    say('connect-msg', `Paired with ${peer.name}. You can connect now.`);
    await renderPeers();
  }),
);

$('connect-btn').addEventListener('click', () =>
  guarded('connect-msg', async () => {
    const address = $('address').value.trim();
    if (!address) throw new Error('Enter the address of the device.');
    const state = await window.tether.connect(address);
    showConnection(state);
    startPolling();
  }),
);

$('disconnect-btn').addEventListener('click', () =>
  guarded('connect-msg', async () => {
    stopPolling();
    showConnection(await window.tether.disconnect());
  }),
);

// ----- accepting connections -----
function buildGrantBoxes() {
  $('grant-boxes').replaceChildren(
    ...PERMS.map((p) => {
      const box = el('input', { type: 'checkbox', value: p, checked: p === 'view' });
      return el('label', {}, box, PERM_LABEL[p]);
    }),
  );
}

function chosenGrant() {
  return [...$('grant-boxes').querySelectorAll('input:checked')].map((i) => i.value);
}

function showHost(state) {
  $('host-form').hidden = state.running;
  $('host-running').hidden = !state.running;
  if (!state.running) return;
  $('host-address').textContent = state.address;
  $('code-block').hidden = !state.code;
  $('host-code').textContent = state.code ?? '';
  showQr(state.links ?? []);
  $('host-log').replaceChildren(
    ...state.log.slice().reverse().map((e) => el('li', { textContent: e.message })),
  );
}

// The QR code shows one address at a time. If this computer has several (Wi-Fi, Ethernet,
// Tailscale) the person picks the one the phone can reach.
let shownLinks = '';
function showQr(links) {
  if (links.length === 0) return;
  const signature = links.map((l) => l.address).join('|');
  const select = $('qr-address');
  if (signature !== shownLinks) {
    shownLinks = signature;
    select.replaceChildren(...links.map((l, i) => el('option', { value: String(i), textContent: l.address })));
    $('qr-address-row').hidden = links.length < 2;
  }
  const chosen = links[Number(select.value) || 0];
  $('host-qr').src = chosen.svgDataUrl;
  $('host-link').textContent = chosen.link;
}
$('qr-address').addEventListener('change', () => pollHostNow());

// Pasting a pairing link into the code box fills in both fields.
$('code').addEventListener('input', async (e) => {
  const text = e.target.value.trim();
  if (!text.startsWith('tether://')) return;
  await guarded('connect-msg', async () => {
    const link = await window.tether.parseLink(text);
    $('address').value = link.address;
    $('code').value = link.code;
    say('connect-msg', link.name ? `Link from ${link.name} read. Press Pair.` : 'Link read. Press Pair.');
  });
});

$('pairing').addEventListener('change', (e) => {
  $('grant-set').disabled = !e.target.checked;
});

$('host-start').addEventListener('click', () =>
  guarded('host-msg', async () => {
    const state = await window.tether.startHost({
      bind: $('bind').value.trim(),
      pairing: $('pairing').checked,
      video: $('video').checked,
      input: $('allow-input').checked,
      grant: chosenGrant(),
    });
    showHost(state);
  }),
);

$('host-stop').addEventListener('click', () =>
  guarded('host-msg', async () => showHost(await window.tether.stopHost())),
);

// ----- paired devices -----
async function renderPeers() {
  const peers = await window.tether.listPeers();
  const root = $('peers');
  if (peers.length === 0) {
    root.replaceChildren(el('p', { className: 'muted', textContent: 'Nobody is paired with this machine yet.' }));
    return;
  }
  const head = el(
    'tr',
    {},
    el('th', { textContent: 'Device' }),
    ...PERMS.map((p) => el('th', { className: 'perm', textContent: PERM_LABEL[p] })),
    el('th'),
  );
  const rows = peers.map((peer) => {
    const nameCell = el('td', {}, el('strong', { textContent: peer.name }), ' ', el('span', { className: 'muted', textContent: peer.platform }));
    nameCell.append(el('div', {}, el('code', { textContent: peer.fingerprint })));
    const cells = PERMS.map((p) => {
      const box = el('input', { type: 'checkbox', checked: peer.permissions.includes(p), ariaLabel: `${PERM_LABEL[p]} for ${peer.name}` });
      box.addEventListener('change', () =>
        guarded('peers-msg', async () => {
          const call = box.checked ? window.tether.grant : window.tether.revoke;
          await call(peer.fingerprint, [p]);
        }).finally(renderPeers),
      );
      return el('td', { className: 'perm' }, box);
    });
    const remove = el('button', { className: 'danger', textContent: 'Unpair' });
    remove.addEventListener('click', () =>
      guarded('peers-msg', async () => {
        await window.tether.removePeer(peer.fingerprint);
      }).finally(renderPeers),
    );
    return el('tr', {}, nameCell, ...cells, el('td', {}, remove));
  });
  root.replaceChildren(el('table', {}, el('thead', {}, head), el('tbody', {}, ...rows)));
}

// Keep the host activity and the peer list fresh while the window is open.
async function pollHostNow() {
  try {
    const host = await window.tether.pollHost();
    if (host.running) showHost(host);
  } catch { /* the next tick will try again */ }
}

setInterval(async () => {
  try {
    const host = await window.tether.pollHost();
    if (host.running) showHost(host);
    if (host.log.some((e) => e.kind === 'paired')) renderPeers();
  } catch { /* the next tick will try again */ }
}, 1500);

(async () => {
  buildGrantBoxes();
  await guarded('connect-msg', loadIdentity);
  await guarded('peers-msg', renderPeers);
})();
