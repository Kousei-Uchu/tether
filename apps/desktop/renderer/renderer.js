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
  const video = $('screen');
  video.srcObject = null;
  video.hidden = true;
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

$('show-screen').addEventListener('click', startVideo);
$('stop-screen').addEventListener('click', () => stopVideo(''));

// ----- connecting -----
let pollTimer = null;

function showConnection(state) {
  const box = $('connection');
  box.hidden = !state.connected;
  mayViewNow = state.connected && state.permissions.includes('view');
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
  $('host-log').replaceChildren(
    ...state.log.slice().reverse().map((e) => el('li', { textContent: e.message })),
  );
}

$('pairing').addEventListener('change', (e) => {
  $('grant-set').disabled = !e.target.checked;
});

$('host-start').addEventListener('click', () =>
  guarded('host-msg', async () => {
    const state = await window.tether.startHost({
      bind: $('bind').value.trim(),
      pairing: $('pairing').checked,
      video: $('video').checked,
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
