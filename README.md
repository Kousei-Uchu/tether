# Tether

Remote desktop and device control between Windows, macOS and iPhone, built from scratch.

This repository is at **step 3 of 8, with the iPhone app started**: a shared protocol, one-time-code pairing, an
encrypted, mutually authenticated connection between two machines, per-device permissions,
an Electron desktop app, live video over WebRTC, and remote mouse and keyboard control, and clipboard text sync. The
video source is a **test pattern** for now; capturing a real screen comes after the iPhone app.
The iPhone app (a trackpad, keyboard and media controller that pairs by scanning a QR code) is
written but has not been compiled yet; see [apps/ios/README.md](apps/ios/README.md).
The full plan, requirements and progress live in [docs/SPEC.md](docs/SPEC.md).

## Layout

```
crates/
  tether-proto   wire types and codec (shared by every client)
  tether-core    identity, trust store, pairing, Noise secure channel, sessions
  tether-cli     the `tether` command, a terminal front end for the same features
  tether-media   host side video: frame sources, H.264 encoder, WebRTC
  tether-input   host side mouse and keyboard injection (enigo)
  tether-clipboard host side clipboard text read and write (arboard)
  tether-ffi     the client side of the core for Swift, through UniFFI
  tether-node    Node and Electron bridge to tether-core (napi-rs)
apps/
  desktop        the Electron app
  ios            the iPhone app (SwiftUI), see apps/ios/README.md
docs/SPEC.md     specification, security model, milestones, decision log
```

`tether-core` has no platform specific code. Later steps add host agent and client crates
next to these.

## Build and test

Needs Rust 1.80 or newer (`rustup` on Windows and macOS) and a C++ compiler for the H.264
encoder: Xcode command line tools on macOS, the Visual Studio C++ build tools on Windows.
The first build compiles WebRTC and OpenH264 and takes several minutes; later builds are fast.
If the build complains about `nasm`, install it (`brew install nasm`, `choco install nasm`).

```
cargo build --release
cargo test --workspace
```

The binary is `target/release/tether` (`tether.exe` on Windows).

## Desktop app

Needs Node 22 or newer as well as Rust.

```
cd apps/desktop
npm install
npm start
```

`npm start` builds the Rust bridge first, then opens the window. From there you can pair
with another device using its code, connect and watch round trip time and permissions, accept
connections on this machine (showing a pairing code), and manage what each paired device is
allowed to do.

To see video: on the host, leave "Share a test video" ticked and start accepting
connections. On the other device, connect and press **Show screen**. The button is disabled
unless the host has granted `view`, and the video stops the moment `view` is revoked.
To pair a phone, leave pairing on when starting to accept connections: the window shows a QR
code for each address the computer can be reached at (pick the one the phone can reach if
there are several). The same link can be pasted into another computer's pairing code box.

To control the host: it must opt in, because this moves its real mouse. Tick **Allow remote
control** when starting it (or pass `--input` to `tether listen`), and give the device the
`input` permission. On the other device, tick **Control this computer** under the video, move
the pointer over it, and click it before typing. Click anywhere else to take your keyboard
back. Media keys need the `media` permission instead of `input`. Revoking either permission
takes control away at once, and releases any key that was held down.

To share clipboards: the host must opt in, because this exposes whatever is on its clipboard (passwords copied from a password manager, say). Tick **Allow clipboard sharing** when starting it (or pass `--clipboard` to `tether listen`), and give the device the `files` permission. The connection card then shows **Send my clipboard** and **Get its clipboard**. Only text is copied, up to 1 MiB, and only when you press a button; nothing syncs by itself. From the terminal:

```
tether clip send <host-address> some text      # or: echo text | tether clip send <host-address>
tether clip get <host-address>
```

To find computers without typing an address: a host announces itself on the local network (Bonjour, service `_tether._tcp`) while it is accepting connections, unless you pass `--no-advertise` to `tether listen`. **Look for devices** on the desktop's Computers page, `tether discover`, and the iPhone's "On this network" list show what is there and which you have paired. If a computer you know has changed address, **Connect** (and the iPhone's tap-to-connect) looks for it by fingerprint and uses the new address; from the terminal, `tether quick <name-or-fingerprint>` does the same. The announcement carries the device name, platform and public key, and is not trusted: connecting still verifies the key in the handshake. Some routers block multicast between Wi-Fi and Ethernet; connect by address then. The iPhone asks for Local Network access the first time.

Pairing checks that nobody is in the middle: after the code is accepted, both devices show the same seven symbols (and the same number, for terminals) and each person confirms they match. Nothing is stored until both have said yes. If the symbols differ, choose "don't match" and try again on a network you trust. From the terminal, `tether pair` and `tether listen --pair` ask on the spot; `--no-verify` trusts the code alone. To approve each connection from an already paired device, tick **Ask before each connection** when starting to share, or pass `--confirm` to `tether listen` (it needs a terminal, so it does not work under `tether service`).

To share files, start the host with a folder: `tether listen --share ~/Tether` (the desktop app has a **Shared folder** setting). Devices with the `files` permission can then browse that folder and download from it, and upload into it, from the desktop app, the iPhone's folder button, or `tether files ls|get|put <address>`. They cannot reach anything outside the folder, and an upload never replaces a file with the same name: it is saved as `name (1).ext` instead. A download is only saved once the whole file has arrived and matches its checksum.

Platform notes for the host:

- macOS also asks for Screen Recording the first time a viewer connects. Allow it under System Settings, Privacy & Security, Screen Recording, then restart Tether.
- Large displays are scaled down to fit 1920×1080 before encoding, because the software encoder cannot keep up with more.
- macOS asks for Accessibility access the first time. Allow it under System Settings, Privacy
  & Security, Accessibility, for Tether (or Electron while developing), then try again.
- Windows cannot reach elevated windows, the UAC prompt or the lock screen unless Tether runs
  as administrator.
- Linux needs an X11 session. Wayland is not supported yet.
- Keys are sent as physical positions on a US keyboard, so a host with another layout types
  what that layout produces for those positions.

The test pattern is a gradient with a bouncing square and a row of black and white blocks
along the bottom that count frames in binary. The same data directory is used as by the `tether` command, so the two
interoperate.

Checks that need no display on Windows and macOS, and `xvfb-run -a` on Linux:

```
npm run smoke      # the window opens and shows this device's identity
npm run e2e        # drives the real UI through pair, connect, video, live grant and revoke
cd ../../crates/tether-node && npm test   # the bridge on its own, no Electron
xvfb-run -a cargo test -p tether-input -p tether-media -- --include-ignored   # real X display: input injection, screen capture (Linux)
scripts/ffi-check.sh                    # the phone's Rust interface, driven from Python against a real host
```

On Linux as root (containers) Electron also needs `--no-sandbox`.

## Try it from the terminal

Pairing needs one device to listen and another to pair with it. Both can be on the same
machine if you give them separate data directories.

On the host:

```
tether listen --pair
```

It prints its fingerprint and a code such as `4XA6-VVF2-M00G-9YHW`. The device that pairs
with it gets view-only access unless you choose otherwise, for example
`tether listen --pair --grant view,input,media`. On the other device:

```
tether pair <host-address> --code 4XA6-VVF2-M00G-9YHW
```

The address is `host` or `host:port`. The default port is 7420. After pairing, the code is
spent and the host stops offering pairing. From then on:

```
tether connect <host-address>
```

opens an authenticated session, prints what the host allows this device to do, and prints
round trip times. Pairing is mutual, so unpair on both devices.

To try everything on one machine:

```
tether --home /tmp/host listen --bind 127.0.0.1:7499 --pair
tether --home /tmp/client pair 127.0.0.1:7499 --code <code>
tether --home /tmp/client connect 127.0.0.1:7499
```

To serve the test video from the terminal too, add `--video test-pattern` to `listen`, and
`--input` to allow remote control.

Set `RUST_LOG=debug` for more detail.

## Every screen in its own window

When you are connected to a computer with several monitors, "Open every screen in its own window" in the desktop app opens one window per monitor, each with its own video, so you can put them on your own monitors. Tick "Control this screen" in a window and your mouse acts on that monitor of the other computer. Keyboard and scroll go wherever the pointer is on the other end, as usual. It needs `view`, and `input` to control.

The older single view ("Show screen") still shows only the main monitor. Each extra screen costs another encode on the host, so a laptop sharing three monitors works noticeably harder than one sharing one. With `--video test-pattern` the host pretends to have two screens, which is handy for trying this out.

## Sound

A host can send what the computer is playing along with the video: `tether listen --video screen --audio system`, or the "Also share this computer's sound" box in the desktop app (on by default). It travels as Opus (stereo, 96 kbps) on the same connection as the video, only to viewers that ask for it. The desktop app asks by default; untick "Play its sound" to skip it. When one host's screens are opened in separate windows, only the first window plays the sound, and it has a Mute box.

What it needs on the host:

- Windows: nothing extra (WASAPI loopback).
- macOS: 14.2 or later, and the system audio recording permission (macOS asks the first time; it is a different permission from Screen Recording). macOS 13 to 14.1 cannot capture system sound yet.
- Linux: a PulseAudio or PipeWire "monitor" input, which most desktops have. Building needs `libasound2-dev`.

The sound goes only from the host to the viewer so far. Sending your own microphone or sound the other way, and keeping the two from feeding back, comes later. With `--audio tone` the host sends a steady 440 Hz tone, handy for trying it without a sound card.

## Media keys

A device with the `media` permission gets a row of play, pause, track and volume buttons in the desktop app and on the iPhone. They press the real media keys on the other computer, so the host needs remote control turned on (`--input` or the checkbox), and the key presses go to whatever app that computer sends them to.

## Waking a sleeping computer

Magic packets do not cross the internet, so something that stays on, on the same network as the computer, has to send them. Any Tether host can do it. On a Raspberry Pi:

```
tether service install --wake desk=aa:bb:cc:dd:ee:ff@192.168.1.255
```

Pair your devices with the Pi and give them `power` (`peers grant`). Then from anywhere you can reach the Pi (Tailscale works):

```
tether wake pi.local desk
```

The part after `@` is the network's broadcast address; leave it out to use 255.255.255.255. The desktop app has a Wake button for the same thing (shown once you hold `power`). Step by step setup, including the BIOS, adapter and macOS settings, is in [docs/WAKE.md](docs/WAKE.md). The computer needs Wake-on-LAN turned on in its BIOS and network adapter settings, and should be on Ethernet. A Mac wakes from sleep only (not shut down), and Wi-Fi wake is unreliable on both systems.

## Running in the background

```
tether service install --video screen --input   # start at login, keep it running
tether service status
tether service uninstall
```

`--dry-run` prints what would be installed. It is a per user service on every platform, because screen capture and input need your desktop session: a systemd user unit on Linux, a launch agent on macOS (log in `~/Library/Logs/tether.log`), and a logon task named Tether on Windows. Pair devices first with `tether listen --pair` or the desktop app, since a background host does not show pairing codes. On Linux, run `install` from your desktop session so the display is passed on. Install again to change the options.

## Permissions

Each paired device has its own permissions on the host it connects to: `view`, `input`,
`media`, `power`, `files`. The host decides, and a device can never widen its own access.

```
tether peers list                      # who is paired and what they can do
tether peers grant laptop input,media  # add permissions
tether peers revoke laptop input       # take permissions away
tether peers remove laptop             # unpair completely
```

`laptop` is the device's name or the start of its fingerprint. Changes apply to a running
`tether listen` immediately, including to sessions that are already open. Devices paired
before permissions existed load as view only.

## Where things are stored

`--home` or the `TETHER_HOME` environment variable overrides the location. By default:

| OS | Location |
| --- | --- |
| Windows | `%APPDATA%\tether\data` |
| macOS | `~/Library/Application Support/tether` |
| Linux | `~/.local/share/tether` |

Inside: `identity.json` (this device's private key, keep it secret) and `trusted.json`
(the devices allowed to connect).

## Security in brief

- Every device has its own key pair. There are no passwords and nothing is trusted on
  first use.
- Connections use Noise XX, so both sides authenticate each other and everything is
  encrypted.
- Pairing uses a single-use code with 80 bits of entropy that expires after five minutes and
  dies after five wrong guesses. Proofs of the code are bound to the specific encrypted
  session, so a man in the middle cannot reuse them.
- Do not expose port 7420 to the internet. Use Tailscale or stay on your LAN.

Details and known limits are in section 5 of the spec.

## Packaging

Each script builds for the machine it runs on, because the Rust parts are native code. The release workflow (`.github/workflows/release.yml`) runs them on Windows, macOS (Apple Silicon and Intel) and Linux, from the Actions tab or by pushing a tag like `v0.1.0`, which also attaches the files to a GitHub release.

| Script | Produces |
| --- | --- |
| `scripts/build-desktop.sh` | macOS: `.dmg`, `.zip`. Linux: `.AppImage`, `.deb`. In `apps/desktop/dist/` |
| `scripts/build-desktop.ps1` | Windows: installer `.exe`, `.zip`. In `apps\desktop\dist\` |
| `scripts/build-cli.sh`, `scripts/build-cli.ps1` | the `tether` command, in `dist/` |
| `scripts/build-ios.sh` | the iPhone library (`TetherFFI.xcframework`), Mac only |

Builds are unsigned for now, so macOS and Windows will warn on first launch. Signing needs certificates and is a later step.

## Applying later steps

Each following step arrives as a patch file made against the previous step. From the
repository root, apply, commit and push in one go (adjust the file name and message):

```
git apply --index ~/Downloads/step-2.patch && git commit -m "Step 2: <title>" && git push
```

Apply patches in order. If `git apply` complains, check that `git status` is clean and the
previous step is applied.

## Renaming the project

`Tether` is a working name. To rename, change the crate names in each `Cargo.toml` and the Node package names, the
`[[bin]]` name in `crates/tether-cli/Cargo.toml`, the `ProjectDirs` string in
`crates/tether-core/src/paths.rs`, and the `tether/7` prologue in `crates/tether-proto`
(changing the prologue breaks compatibility with older builds, which is fine before release).
The prologue is `tether/<protocol version>`, so keep the two in step.
