import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createRequire } from 'node:module';

const { Device } = createRequire(import.meta.url)('../index.js');

function makeDevice(name) {
  const home = mkdtempSync(join(tmpdir(), `tether-${name}-`));
  return { device: new Device(home, name), home };
}

test('pair, connect, live permission changes, unpair', async () => {
  const host = makeDevice('host-pc');
  const client = makeDevice('laptop');
  try {
    assert.match(host.device.fingerprint, /^([0-9a-f]{4}-){7}[0-9a-f]{4}$/);
    assert.equal(host.device.name, 'host-pc');

    const handle = await host.device.listen('127.0.0.1:0', { grant: ['view', 'media'] });
    assert.match(handle.pairingCode, /^[0-9A-Z]{4}(-[0-9A-Z]{4}){3}$/);

    // Wrong code is refused and nothing is stored.
    await assert.rejects(client.device.pair(handle.address, 'AAAA-BBBB-CCCC-DDDD'), /pairing failed/);
    assert.equal(client.device.peers().length, 0);

    // Connecting before pairing is refused.
    await assert.rejects(client.device.connect(handle.address), /not paired/);

    const paired = await client.device.pair(handle.address, handle.pairingCode);
    assert.equal(paired.name, 'host-pc');
    assert.deepEqual(host.device.peers().map((p) => [p.name, p.permissions]), [
      ['laptop', ['view', 'media']],
    ]);

    const conn = await client.device.connect(handle.address);
    assert.equal(conn.peer.name, 'host-pc');
    assert.deepEqual(conn.permissions, ['view', 'media']);
    assert.ok((await conn.ping()) >= 0);

    // The host widens, then narrows, the grant while the session is open.
    host.device.grant('laptop', ['input', 'power']);
    await conn.ping();
    assert.deepEqual(conn.permissions, ['view', 'input', 'media', 'power']);
    host.device.revoke('laptop', ['power']);
    await conn.ping();
    assert.deepEqual(conn.permissions, ['view', 'input', 'media']);

    // Unpairing ends the session, and the connection reports why.
    assert.equal(host.device.remove('laptop'), 1);
    await assert.rejects(conn.ping(), /access revoked/);
    await assert.rejects(conn.ping(), /closed/);
    await assert.rejects(client.device.connect(handle.address), /not paired/);

    const kinds = handle.drainEvents().map((e) => e.kind);
    assert.ok(kinds.includes('paired'));
    assert.ok(kinds.includes('session-opened'));
    assert.ok(kinds.includes('refused'));
    assert.deepEqual(handle.drainEvents(), []);

    handle.stop();
  } finally {
    rmSync(host.home, { recursive: true, force: true });
    rmSync(client.home, { recursive: true, force: true });
  }
});

test('bad input produces readable errors', async () => {
  const d = makeDevice('solo');
  try {
    assert.throws(() => d.device.grant('nobody', ['view']), /no paired device matches/);
    assert.throws(() => d.device.grant('nobody', ['fly']), /unknown permission/);
    await assert.rejects(d.device.connect('127.0.0.1:1'), /could not connect/);
  } finally {
    rmSync(d.home, { recursive: true, force: true });
  }
});

test('stopping the host closes the port', async () => {
  const host = makeDevice('h');
  const client = makeDevice('c');
  try {
    const handle = await host.device.listen('127.0.0.1:0', { grant: ['view'] });
    await client.device.pair(handle.address, handle.pairingCode);
    const conn = await client.device.connect(handle.address);
    handle.stop();
    await new Promise((r) => setTimeout(r, 200));
    await assert.rejects(conn.ping());
    await assert.rejects(client.device.connect(handle.address), /could not connect/);
  } finally {
    rmSync(host.home, { recursive: true, force: true });
    rmSync(client.home, { recursive: true, force: true });
  }
});

test('video requests are refused without a source or without view', async () => {
  const host = makeDevice('h2');
  const client = makeDevice('c2');
  try {
    // A host that shares no video says so.
    let handle = await host.device.listen('127.0.0.1:0', { grant: ['view'] });
    await client.device.pair(handle.address, handle.pairingCode);
    let conn = await client.device.connect(handle.address);
    await assert.rejects(conn.requestVideo('v=0'), /not sharing/);
    await conn.close();
    handle.stop();

    // A host that shares video still refuses a device that may not view.
    handle = await host.device.listen('127.0.0.1:0', null, 'test-pattern');
    host.device.revoke('c2', ['view']);
    host.device.grant('c2', ['media']);
    conn = await client.device.connect(handle.address);
    assert.deepEqual(conn.permissions, ['media']);
    await assert.rejects(conn.requestVideo('v=0'), /view/);
    // The refusal does not end the session.
    assert.ok((await conn.ping()) >= 0);
    handle.stop();

    await assert.rejects(host.device.listen('127.0.0.1:0', null, 'webcam'), /unknown video source/);
  } finally {
    rmSync(host.home, { recursive: true, force: true });
    rmSync(client.home, { recursive: true, force: true });
  }
});

test('input events are validated, gated, and refusals come back as notices', async () => {
  const host = makeDevice('h3');
  const client = makeDevice('c3');
  try {
    // Remote control is off on this host, and the device holds only `view`.
    const handle = await host.device.listen('127.0.0.1:0', { grant: ['view'] });
    await client.device.pair(handle.address, handle.pairingCode);
    const conn = await client.device.connect(handle.address);

    // Bad events never leave the machine.
    await assert.rejects(conn.sendInput({ kind: 'key', code: 'NotAKey', down: true }), /unsupported key/);
    await assert.rejects(conn.sendInput({ kind: 'wat' }), /unknown input kind/);
    await assert.rejects(conn.sendInput({ kind: 'button', button: 'left' }), /missing `down`/);
    await assert.rejects(conn.sendInput({ kind: 'move', x: 0.5 }), /missing `y`/);

    // Without the permission the host drops the event and says so, once.
    for (let i = 0; i < 3; i++) await conn.sendInput({ kind: 'key', code: 'KeyA', down: true });
    await conn.ping();
    const first = await conn.takeNotices();
    assert.equal(first.length, 1);
    assert.match(first[0], /not allowed.*input/);
    assert.deepEqual(await conn.takeNotices(), []);

    // With the permission but remote control off on the host, the notice says that instead.
    host.device.grant('c3', ['input']);
    await conn.ping();
    await conn.sendInput({ kind: 'move', x: 0.5, y: 0.5 });
    await conn.ping();
    const second = await conn.takeNotices();
    assert.equal(second.length, 1);
    assert.match(second[0], /turned off/);

    handle.stop();
  } finally {
    rmSync(host.home, { recursive: true, force: true });
    rmSync(client.home, { recursive: true, force: true });
  }
});
