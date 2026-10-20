# Tether: Specification and Progress Tracker

Cross-platform remote desktop and device control between a Windows PC, a Mac and an iPhone.
This document is the source of truth for scope, design decisions and progress. Update it in
the same commit as the code it describes.

| Field | Value |
| --- | --- |
| Status | Milestone 2 of 8 complete, milestone 3 in progress (3a done) |
| Protocol version | 2 |
| Targets | Windows 10+, macOS 13+, iOS 17+ |
| Core language | Rust (agent, protocol, crypto), SwiftUI (iPhone), Electron (desktop UI) |

## 1. Goals and non-goals

### Goals

- G-1. View and control one machine from another, with each remote display shown as its own
  window that can be docked next to the others.
- G-2. Control mouse, keyboard, media and settings of a remote machine from a PC, a Mac or
  an iPhone.
- G-3. Wake a sleeping machine remotely.
- G-4. Stream audio in both directions.
- G-5. Be safe by default: no passwords, no open ports on the public internet, every
  device authenticated by public key.

### Non-goals (for now)

- Android and Linux clients.
- Controlling the macOS login screen (not feasible with public APIs).
- Waking a Mac from hibernate or from a full power off (hardware limitation).
- A hosted relay service. Tailscale or a LAN is assumed until a self-hosted relay is added.

## 2. Prior art to study before each milestone

| Project | Why it matters |
| --- | --- |
| RustDesk | Closest open source equivalent. Reference for capture, input and service-vs-session handling on Windows. |
| Sunshine and Moonlight | Best low-latency streaming path. Reference for hardware encoding and input latency. |
| Deskflow and Input Leap | Reference for edge-crossing mouse and keyboard sharing. |

## 3. Requirements and status

Legend: `[x]` done, `[~]` in progress, `[ ]` not started.

| ID | Requirement | Milestone | Status |
| --- | --- | --- | --- |
| R-1 | Devices pair once with a one-time code and public-key identity | M1 | [x] |
| R-2 | All traffic is mutually authenticated and encrypted | M1 | [x] |
| R-3 | Remote view of one display, PC to Mac and Mac to PC | M3 | [ ] |
| R-4 | Mouse and keyboard injection on both OSes | M3 | [ ] |
| R-5 | System audio streaming, both directions | M4 | [ ] |
| R-6 | iPhone trackpad, keyboard and media controller | M5 | [ ] |
| R-7 | Wake from sleep through an always-on LAN relay | M6 | [ ] |
| R-8 | One window per remote display, with snapping and saved layout | M7 | [ ] |
| R-9 | Cursor flows across screen edges between machines | M7 | [ ] |
| R-10 | Clipboard sync and file transfer | M8 | [ ] |
| R-11 | Privacy mode, virtual display, adaptive bitrate | M8 | [ ] |
| R-12 | Notification forwarding, macros, system stats | M8 | [ ] |
| R-13 | Per-device permissions, enforced by the host and changeable while connected | M2 | [x] |

## 4. Architecture

### 4.1 Components

| Component | Responsibility | Tech |
| --- | --- | --- |
| Host agent | Capture screen and audio, encode, inject input, run power and settings commands | Rust, one core with thin per-OS modules |
| Desktop client | Render remote displays as windows, manage layout | Electron |
| iPhone client | Controller UI, optional viewer | SwiftUI |
| Wake relay | Always-on LAN device that turns a wake request into a magic packet | Rust on a Raspberry Pi |
| `tether-proto` | Wire types, shared by every component | Rust crate, ported to Swift later |
| `tether-core` | Identity, trust store, pairing, secure channel | Rust crate |
| `tether-node` | Exposes `tether-core` to JavaScript: pair, connect, host, permissions | Rust cdylib using napi-rs, loaded by Electron |

### 4.1.1 Desktop client notes

Electron ships Chromium, so the client gets a full WebRTC stack and hardware video decode
for free: remote displays are `RTCPeerConnection` video tracks drawn into one
`BrowserWindow` per display. The Rust side (`tether-core`) still owns identity, pairing, the
control channel and the permission model. Electron reaches it through `tether-node`, a
native addon loaded in the main process (D-11). The renderer has no Node access: it calls a
fixed API exposed by the preload script, which forwards to the main process over IPC.

### 4.2 Transport layers

1. **Control channel (built in M1).** TCP carrying a Noise XX session. Carries pairing,
   authentication, commands, clipboard and, in M2, the WebRTC signaling.
2. **Media (M2 onward).** WebRTC for video and Opus audio. The offer and answer, including
   DTLS fingerprints, travel over the authenticated control channel, so media inherits the
   control channel's authentication and no public signaling server is needed.
3. **Reachability.** Tailscale on every device is the supported path. LAN works directly.
   A self-hosted STUN and TURN setup is a later option.

### 4.3 Platform implementation notes

| Area | Windows | macOS |
| --- | --- | --- |
| Screen capture | Windows Graphics Capture or DXGI Desktop Duplication | ScreenCaptureKit |
| Video encode | NVENC, QuickSync or AMF, H.264 or HEVC | VideoToolbox |
| System audio capture | WASAPI loopback | ScreenCaptureKit audio (macOS 13+) |
| Input injection | `SendInput` | `CGEvent` (needs Accessibility and Screen Recording permission) |
| Lock screen and UAC | Needs a SYSTEM service plus a helper in the user session | Not controllable |
| Virtual audio device | VB-Cable or similar | BlackHole or similar |

Each remote display is a separate video track. Docking is client side window snapping plus
a saved layout. Edge-crossing input (R-9) needs virtual display geometry on the client and
input forwarding when the cursor leaves an edge.

### 4.4 Wake from sleep (R-7)

| Fact | Consequence |
| --- | --- |
| Wake-on-LAN works from sleep on both platforms | Supported target |
| Windows can often wake from hibernate or off, if BIOS and NIC allow it and it is on Ethernet | Supported where hardware allows |
| Macs wake from sleep only, and Wi-Fi wake is unreliable on both OSes | Configure the Mac to sleep, not hibernate |
| Magic packets do not cross the internet | Needs an always-on LAN device that receives the request and broadcasts the packet |

### 4.5 Audio (R-5)

- Send as Opus over WebRTC.
- Received audio plays to the speakers by default. Exposing it as a microphone on the other
  machine needs a virtual audio driver.
- Both-way mirroring can loop. Exclude the app's own output from capture and apply echo
  cancellation.

## 5. Security model and wire protocol (implemented in M1)

### 5.1 Identity and trust

- Each device has a long-term X25519 key pair, created on first run and stored in
  `identity.json` (file mode 0600 on Unix).
- A device identifies itself by the fingerprint of its public key: SHA-256, first 16 bytes,
  hex, in eight groups of four.
- A host accepts normal sessions only from public keys in its trust store. A client
  connects only to hosts in its trust store. There is no password and no trust on first use.
- Each trusted device on a host has a permission set (section 5.6). The entry for a host on
  a client is identity only.

### 5.2 Secure channel

- Noise `XX_25519_ChaChaPoly_BLAKE2s`, prologue `tether/2` (tracks the protocol version). XX authenticates both static
  keys and hides them from passive observers.
- Framing: `u16` big endian length, then one Noise transport frame. Plaintext of a frame is
  one flag byte (0 more, 1 last) plus up to 65518 bytes of message. Messages are chunked
  and reassembled. Maximum message size is 16 MiB.
- Send and receive halves are independent (snow stateless transport with per-direction
  nonce counters), so they can be driven from separate tasks.

### 5.3 Pairing

1. The host runs `listen --pair` and shows a one-time code: 16 Crockford base32 characters,
   80 bits, valid for 5 minutes by default, one use, five wrong guesses then it dies.
2. The client connects, completes the Noise handshake, exchanges `Hello` (intent `Pair`) and
   sends `PairRequest { proof }` where `proof = HMAC-SHA256(code, "tether-pair-v1/client" ||
   handshake_hash)`.
3. The host verifies in constant time and answers `PairAccept { proof }` with the same
   construction using the label `tether-pair-v1/host`. Both sides then store the other's key.
4. The proofs are bound to the Noise handshake hash, so a relaying attacker produces
   different hashes on each leg and both proofs fail. The code is long enough that a
   captured proof cannot be brute forced offline.

### 5.4 Messages (protocol version 1)

Encoding is `postcard`. Variant order is the wire format: append new variants only, and
bump `PROTOCOL_VERSION` for any incompatible change. A unit test pins the indices.

| Index | Message | Direction | Purpose |
| --- | --- | --- | --- |
| 0 | `Hello` | both | Version, device name, platform, intent (`Connect` or `Pair`) |
| 1 | `PairRequest` | client to host | Client proof of the pairing code |
| 2 | `PairAccept` | host to client | Host proof, completes pairing |
| 3 | `Reject` | either | Refusal with a reason |
| 4 | `Ready` | host to client | Paired client authenticated |
| 5 | `Ping` | either | Liveness and round trip time |
| 6 | `Pong` | either | Reply to `Ping` |
| 7 | `Bye` | either | Clean close |
| 8 | `Permissions` | host to client | What the client may do. Sent after `Ready` and again whenever the grant changes |

### 5.5 Known limits

- No key rotation or revocation sync. Removing a peer is a local action on each device.
- No rekeying within a session. Acceptable at this volume, revisit when streaming bulk data
  over the control channel.
- Pairing codes are shown in a terminal. QR code pairing arrives with the iPhone client.
- A client only learns its permissions from the host. They are advisory for UI purposes;
  the host is the only place they are enforced.

### 5.6 Permissions

| Permission | Covers |
| --- | --- |
| `view` | See the screen and hear the audio |
| `input` | Mouse, keyboard and touch input |
| `media` | Media keys, volume, brightness |
| `power` | Lock, sleep, restart, shutdown, wake |
| `files` | Clipboard sync and file transfer |

- The host picks the grant when it shows a pairing code (`listen --pair --grant ...`). The
  default is `view` only: look, don't touch.
- The grant lives in the host's trust store. `peers grant` and `peers revoke` change it
  and `peers remove` unpairs the device. All three take effect on a running host.
- Enforcement is on the host, per request. Every permissioned action must go through
  `Session::authorize(host, permission)`, which reloads the trust file if it changed,
  tells the client about any change, ends the session if the device was unpaired, and then
  checks the permission. Nothing is cached across requests in a way that outlives a revoke.
- Trust files written before M2 have no permission field. Those devices load as `view`.
- Pairing as a client also stores the host in the trust list, with no permissions. The
  desktop app lists it with every box unchecked. It is the host's identity record, and it
  only matters if that device later connects here, when it could do nothing.

## 6. Milestones

### M1. Protocol, pairing, encrypted connection (done)

- [x] Workspace with `tether-proto`, `tether-core`, `tether-cli`
- [x] Wire types and codec, with stability tests
- [x] Device identity and trust store
- [x] Noise XX secure channel with chunking
- [x] One-time code pairing bound to the session
- [x] Authenticated session with ping and pong
- [x] CLI: `id`, `listen`, `pair`, `connect`, `peers`
- [x] Tests: 17 passing, including pairing failure, expiry, attempt limits, unpaired and
      unknown peers
- [x] CI on Windows, macOS and Linux

Acceptance: two machines pair with a code, reconnect without it, and refuse everyone else.

### M2. Per-device permissions (done)

- [x] `Permission` type and `Permissions` message, protocol version 2
- [x] Permission sets stored per device in the host trust store
- [x] Grant chosen at pairing time, default view only
- [x] `tether peers grant`, `peers revoke`, `peers list` showing permissions
- [x] Host reloads the trust file when it changes, so grants and unpairing apply live
- [x] `Session::authorize` gate for later permissioned actions
- [x] Tests: grant at pairing, default grant, live grant and revoke, unpairing ends session,
      authorize gate, legacy trust files, no clobbering of concurrent edits (26 tests total)

Acceptance: a view-only device cannot do anything permissioned, and the host changes that
without restarting or re-pairing.

### M3a. Electron shell and Rust bridge (done)

- [x] `tether-node` addon: device identity, peers, grant/revoke/remove, pair, connect, ping,
      host with pairing code and an event feed
- [x] Hand-written TypeScript typings and a build script that works on Windows, macOS, Linux
- [x] Electron app with a locked-down renderer (context isolation, sandbox, CSP, no navigation)
- [x] UI for pairing, connecting, live permission display, hosting, and managing paired devices
- [x] Tests: bridge tests in Node (3), smoke test and full UI end-to-end test in Electron
- [x] CI runs the bridge tests on Windows, macOS and Linux

Acceptance: everything the CLI can do is usable from the window, and a permission change on
the host shows up in the client window without reconnecting.

### M3b. Single display video and input injection

- [ ] Host agent as a background service skeleton (Windows service, macOS launch agent)
- [ ] Capture one display, hardware encode, send over WebRTC negotiated on the control channel
- [ ] Desktop client window that renders the stream
- [ ] Input injection: mouse, keyboard, scroll, gated by `view` and `input`
- [ ] PC to Mac first, then Mac to PC

Acceptance: control a remote desktop with under 100 ms input-to-pixel on a LAN.

### M4. System audio

- [ ] WASAPI loopback and ScreenCaptureKit audio capture
- [ ] Opus track both ways, feedback loop prevention
- [ ] Optional virtual device routing

### M5. iPhone controller

- [ ] SwiftUI app, Swift port of `tether-proto`, Noise implementation
- [ ] QR code pairing
- [ ] Touch trackpad, keyboard, media panel, controls disabled when the grant lacks them
- [ ] Shortcuts integration

### M6. Wake relay

- [ ] Relay service for a Raspberry Pi: authenticated wake request in, magic packet out
- [ ] Wake button in desktop and iPhone clients, gated by `power`
- [ ] Setup guide for BIOS, NIC and macOS power settings

### M7. Multi-display windows and docking

- [ ] One track and one window per physical display
- [ ] Window snapping and saved layouts
- [ ] Edge crossing of the cursor between machines

### M8. Extras

- [ ] Clipboard sync and drag-and-drop file transfer, gated by `files`
- [ ] Privacy mode and virtual display driver
- [ ] Adaptive bitrate and low-latency toggle
- [ ] Notification forwarding, send-to-device, macros, system stats and task view

## 7. Decision log

| ID | Decision | Rationale | Status |
| --- | --- | --- | --- |
| D-1 | Rust for the agent and shared core | One codebase across Windows and macOS, no runtime, good crypto ecosystem | Accepted |
| D-2 | Tailscale as the first reachability layer | Removes port forwarding, TURN and a public signaling server | Accepted |
| D-3 | Custom Noise control channel, with WebRTC media negotiated over it | One authentication story for everything, no signaling server. The original plan had WebRTC everywhere | Accepted |
| D-4 | 80 bit pairing code, proofs bound to the handshake hash | Avoids a PAKE dependency while staying safe against an active man in the middle | Accepted |
| D-5 | Trust store keyed by public key, no TOFU | A stolen code is single use and expires. A wrong device never silently becomes trusted | Accepted |
| D-6 | postcard encoding, append-only variants | Compact, easy to port to Swift, versioned by a pinned test | Accepted |
| D-7 | Electron for the desktop client | Preferred by the owner. Bundled Chromium gives WebRTC and hardware decode with no extra work | Accepted |
| D-8 | Permissions are per device, enforced on the host, default view only | The host owns its machine. A client must never be able to widen its own access. Least privilege by default | Accepted |
| D-9 | Trust file is the source of truth, reloaded on change | Lets the CLI and later the UI change grants on a running host. Fixes a step 1 gap where a running host kept stale trust | Accepted |
| D-10 | Protocol version 2, prologue tracks the version | `Permissions` is a new message that older peers cannot decode, so versions fail at the handshake instead of mid-session | Accepted |
| D-11 | Electron talks to Rust through an in-process napi-rs addon, not a sidecar | One runtime and one lifecycle, typed calls, no local port to secure. N-API is ABI stable so the addon works in both Node and Electron without per-version rebuilds | Accepted |

## 8. Open questions

- Video frames from Rust to the Electron renderer: only needed if we capture in Rust but decode in Chromium. Plan is WebRTC end to end, so the addon carries signaling and permissions, not pixels. Confirm in M3b.
- Swift Noise implementation: port `snow`'s construction, or use an existing Swift Noise library?
- Do permissions need finer grain later, for example allow-listed apps for `input`, or time-limited grants?
- Self-hosted relay for off-Tailscale use: build, or document Tailscale and stop there?

## 9. Change log

| Date | Change |
| --- | --- |
| 2026-10-19 | M1 complete: protocol, identity, pairing, secure channel, CLI, tests, CI |
| 2026-10-20 | M3a complete: `tether-node` bridge and the Electron desktop app (pair, connect, host, permissions) |
| 2026-10-19 | M2 complete: per-device permissions, live trust reload, protocol version 2. Desktop client set to Electron. Later milestones renumbered |
