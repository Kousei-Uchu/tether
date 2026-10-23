"""Exercises the UniFFI interface from Python against a real `tether listen` host.

Swift gets bindings generated from the same Rust, so what this checks (async calls, objects,
errors, records, enums, notices, live permission changes) is what the iPhone app relies on.
Run it with scripts/ffi-check.sh, which builds everything and starts the host.
"""
import asyncio
import os
import subprocess
import sys

import tether_ffi as t

ADDRESS = os.environ["TETHER_ADDR"]
CODE = os.environ["TETHER_CODE"]
HOST_HOME = os.environ["TETHER_HOST_HOME"]
CLI = os.environ["TETHER_CLI"]
PHONE_HOME = os.environ["TETHER_PHONE_HOME"]


def host(*args):
    """Runs the desktop CLI against the host's data directory, like the host's owner would."""
    out = subprocess.run([CLI, "--home", HOST_HOME, *args], capture_output=True, text=True, check=True)
    return out.stdout


def expect_failure(label, awaitable_or_call, contains):
    async def run():
        try:
            result = awaitable_or_call()
            if asyncio.iscoroutine(result):
                await result
        except t.TetherError.Failed as e:
            assert contains in str(e), f"{label}: {e!s} does not contain {contains!r}"
            return
        raise AssertionError(f"{label}: expected a failure")

    return run()


async def main():
    # Links: parse, build, and refuse nonsense.
    link = t.make_pairing_link("192.168.1.20:7420", "abcd efgh jklm np0o", "Studio PC")
    info = t.parse_pairing_link(link)
    assert (info.address, info.code, info.name) == ("192.168.1.20:7420", "ABCD-EFGH-JK1M-NP00", "Studio PC"), info
    await expect_failure("bad link", lambda: t.parse_pairing_link("https://example.com"), "not a Tether pairing link")

    phone = t.Device(PHONE_HOME, "phone")
    assert phone.name() == "phone" and len(phone.fingerprint()) == 39
    assert phone.peers() == []

    # Not paired yet.
    await expect_failure("connect before pairing", lambda: phone.connect(ADDRESS), "not paired")
    await expect_failure("wrong code", lambda: phone.pair(ADDRESS, "AAAA-BBBB-CCCC-DDDD"), "pairing failed")
    await expect_failure("malformed code", lambda: phone.pair(ADDRESS, "nope"), "invalid pairing code")

    host_peer = await phone.pair(ADDRESS, CODE)
    assert host_peer.name == "ffi-host", host_peer
    assert [p.fingerprint for p in phone.peers()] == [host_peer.fingerprint]
    await expect_failure("code is single use", lambda: phone.pair(ADDRESS, CODE), "")

    conn = await phone.connect(ADDRESS)
    assert conn.peer().name == "ffi-host"
    assert conn.permissions() == ["view", "media"], conn.permissions()
    assert await conn.ping(1) >= 0

    # Bad actions never leave the phone.
    await expect_failure("bad key", lambda: conn.send_input(t.InputAction.KEY(code="Nope", down=True)), "unsupported key")
    await expect_failure("bad button", lambda: conn.send_input(t.InputAction.BUTTON(button="side", down=True)), "unknown mouse button")

    # Without `input` the host drops the action and explains once, in a notice.
    for _ in range(3):
        await conn.send_input(t.InputAction.DELTA(dx=5, dy=5))
    await conn.ping(2)
    notices = await conn.take_notices()
    assert len(notices) == 1 and "not allowed" in notices[0] and "input" in notices[0], notices
    assert await conn.take_notices() == []

    # The host widens the grant while the session is open; the next ping carries it.
    host("peers", "grant", "phone", "input")
    await conn.ping(3)
    assert conn.permissions() == ["view", "input", "media"], conn.permissions()
    # Remote control is off on this host, and the notice says so.
    await conn.send_input(t.InputAction.TEXT(text="hello"))
    await conn.send_input(t.InputAction.SCROLL(dx=0, dy=-3))
    await conn.send_input(t.InputAction.MOVE(x=0.5, y=0.5))
    await conn.ping(4)
    notices = await conn.take_notices()
    assert len(notices) == 1 and "turned off" in notices[0], notices

    # Unpairing ends the session and the reason reaches the phone.
    host("peers", "remove", "phone")
    await expect_failure("revoked", lambda: conn.ping(5), "access revoked")
    await expect_failure("closed after failure", lambda: conn.ping(6), "closed")
    await expect_failure("host forgot us", lambda: phone.connect(ADDRESS), "not paired")

    # The phone can forget the host too.
    assert phone.remove(host_peer.fingerprint[:9]) == 1
    assert phone.peers() == []

    print("FFI OK")


asyncio.run(main())
