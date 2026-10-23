// One window showing one screen of the connected computer. It asks for that screen's own
// video and, when Control is ticked, sends pointer positions aimed at that screen.
const params = new URLSearchParams(location.search);
const displayId = Number(params.get('display'));
document.title = `${params.get('host') || 'Host'}: ${params.get('name') || 'Screen'}`;

const $ = (id) => document.getElementById(id);
const video = $('screen');
const say = (text) => ($('msg').textContent = text);
const send = (event) => window.tether.sendInput(event);

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

async function start() {
  const pc = new RTCPeerConnection({ iceServers: [] });
  try {
    const transceiver = pc.addTransceiver('video', { direction: 'recvonly' });
    const h264 = RTCRtpReceiver.getCapabilities('video').codecs.filter((c) => c.mimeType === 'video/H264');
    if (h264.length === 0) throw new Error('This computer cannot play H.264 video.');
    transceiver.setCodecPreferences(h264);
    pc.ontrack = (event) => {
      video.srcObject = event.streams[0] ?? new MediaStream([event.track]);
      say('');
    };
    pc.onconnectionstatechange = () => {
      if (['failed', 'closed', 'disconnected'].includes(pc.connectionState)) say('The video stopped.');
    };
    await pc.setLocalDescription(await pc.createOffer());
    await waitForIce(pc, 3000);
    const answer = await window.tether.requestDisplayVideo(displayId, pc.localDescription.sdp);
    await pc.setRemoteDescription({ type: 'answer', sdp: answer });
  } catch (e) {
    pc.close();
    say(e.message);
  }
}

// ----- control -----
const heldKeys = new Set();
const heldButtons = new Set();
const BUTTONS = ['left', 'middle', 'right', 'back', 'forward'];
let controlOn = false;
let moveFrame = null;
let lastPoint = null;

function pictureRect() {
  const box = video.getBoundingClientRect();
  if (!video.videoWidth || !video.videoHeight) return null;
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
  if (on) video.focus();
  else releaseAll();
}

const moveTo = (p) => send({ kind: 'move', display: displayId, ...p });

function scheduleMove(point) {
  lastPoint = point;
  if (moveFrame !== null) return;
  moveFrame = requestAnimationFrame(() => {
    moveFrame = null;
    if (controlOn && lastPoint) moveTo(lastPoint);
  });
}

$('control').addEventListener('change', (e) => setControl(e.target.checked));
video.addEventListener('mousemove', (e) => {
  const p = controlOn && toFraction(e);
  if (p) scheduleMove(p);
});
video.addEventListener('mousedown', (e) => {
  const p = controlOn && toFraction(e);
  if (!p) return;
  e.preventDefault();
  video.focus();
  const button = BUTTONS[e.button];
  if (!button) return;
  moveTo(p);
  send({ kind: 'button', button, down: true });
  heldButtons.add(button);
});
window.addEventListener('mouseup', (e) => {
  const button = BUTTONS[e.button];
  if (controlOn && button && heldButtons.delete(button)) send({ kind: 'button', button, down: false });
});
video.addEventListener('contextmenu', (e) => controlOn && e.preventDefault());
video.addEventListener(
  'wheel',
  (e) => {
    if (!controlOn) return;
    e.preventDefault();
    const toLines = (d) => (d === 0 ? 0 : Math.sign(d) * (e.deltaMode === 1 ? Math.abs(d) : Math.max(1, Math.round(Math.abs(d) / 40))));
    send({ kind: 'scroll', dx: toLines(e.deltaX), dy: toLines(e.deltaY) });
  },
  { passive: false },
);
video.addEventListener('keydown', (e) => {
  if (!controlOn) return;
  e.preventDefault();
  heldKeys.add(e.code);
  send({ kind: 'key', code: e.code, down: true });
});
video.addEventListener('keyup', (e) => {
  if (!controlOn) return;
  e.preventDefault();
  heldKeys.delete(e.code);
  send({ kind: 'key', code: e.code, down: false });
});
video.addEventListener('blur', releaseAll);
window.addEventListener('blur', releaseAll);

start();
