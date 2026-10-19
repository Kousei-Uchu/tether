# Tether: Specification and Progress Tracker

Cross-platform remote desktop and device control between a Windows PC, a Mac and an iPhone.
This document is the source of truth for scope, design decisions and progress. Update it in
the same commit as the code it describes.

| Field | Value |
| --- | --- |
| Status | Milestone 1 of 7 complete |
| Protocol version | 1 |
| Targets | Windows 10+, macOS 13+, iOS 17+ |
| Core language | Rust (agent, protocol, crypto), SwiftUI (iPhone), Tauri (desktop UI) |

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
| R-3 | Remote view of one display, PC to Mac and Mac to PC | M2 | [ ] |
| R-4 | Mouse and keyboard injection on both OSes | M2 | [ ] |
| R-5 | System audio streaming, both directions | M3 | [ ] |
| R-6 | iPhone trackpad, keyboard and media controller | M4 | [ ] |
| R-7 | Wake from sleep through an always-on LAN relay | M5 | [ ] |
| R-8 | One window per remote display, with snapping and saved layout | M6 | [ ] |
| R-9 | Cursor flows across screen edges between machines | M6 | [ ] |
| R-10 | Clipboard sync and file transfer | M7 | [ ] |
| R-11 | Privacy mode, virtual display, adaptive bitrate | M7 | [ ] |
| R-12 | Notification forwarding, macros, system stats | M7 | [ ] |

## 4. Architecture

### 4.1 Components

| Component | Responsibility | Tech |
| --- | --- | --- |
| Host agent | Capture screen and audio, encode, inject input, run power and settings commands | Rust, one core with thin per-OS modules |
| Desktop client | Render remote displays as windows, manage layout | Tauri or Electron |
| iPhone client | Controller UI, optional viewer | SwiftUI |
| Wake relay | Always-on LAN device that turns a wake request into a magic packet | Rust on a Raspberry Pi |
| `tether-proto` | Wire types, shared by every component | Rust crate, ported to Swift later |
| `tether-core` | Identity, trust store, pairing, secure channel | Rust crate |

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

### 5.2 Secure channel

- Noise `XX_25519_ChaChaPoly_BLAKE2s`, prologue `tether/1`. XX authenticates both static
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

### 5.5 Known limits of M1

- Trust is all or nothing. There are no per-device permissions yet (for example view only).
- No key rotation or revocation sync. Removing a peer is a local action on each device.
- No rekeying within a session. Acceptable at this volume, revisit when streaming bulk data
  over the control channel.
- Pairing codes are shown in a terminal. QR code pairing arrives with the iPhone client.

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

### M2. Single display video and input injection

- [ ] Host agent as a background service skeleton (Windows service, macOS launch agent)
- [ ] Capture one display, hardware encode, send over WebRTC negotiated on the control channel
- [ ] Desktop client window that renders the stream
- [ ] Input injection: mouse, keyboard, scroll
- [ ] PC to Mac first, then Mac to PC

Acceptance: control a remote desktop with under 100 ms input-to-pixel on a LAN.

### M3. System audio

- [ ] WASAPI loopback and ScreenCaptureKit audio capture
- [ ] Opus track both ways, feedback loop prevention
- [ ] Optional virtual device routing

### M4. iPhone controller

- [ ] SwiftUI app, Swift port of `tether-proto`, Noise implementation
- [ ] QR code pairing
- [ ] Touch trackpad, keyboard, media panel
- [ ] Shortcuts integration

### M5. Wake relay

- [ ] Relay service for a Raspberry Pi: authenticated wake request in, magic packet out
- [ ] Wake button in desktop and iPhone clients
- [ ] Setup guide for BIOS, NIC and macOS power settings

### M6. Multi-display windows and docking

- [ ] One track and one window per physical display
- [ ] Window snapping and saved layouts
- [ ] Edge crossing of the cursor between machines

### M7. Extras

- [ ] Clipboard sync and drag-and-drop file transfer
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

## 8. Open questions

- Which desktop UI stack: Tauri (small, Rust-native) or Electron (more familiar)?
- Swift Noise implementation: port `snow`'s construction, or use an existing Swift Noise library?
- Is a per-device permission model (view only, input, power, files) needed before M2 ships?
- Self-hosted relay for off-Tailscale use: build, or document Tailscale and stop there?

## 9. Change log

| Date | Change |
| --- | --- |
| 2026-10-19 | M1 complete: protocol, identity, pairing, secure channel, CLI, tests, CI |
