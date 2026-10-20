# Tether

Remote desktop and device control between Windows, macOS and iPhone, built from scratch.

This repository is at **step 3 of 8, part a**: a shared protocol, one-time-code pairing, an
encrypted, mutually authenticated connection between two machines, per-device permissions,
and an Electron desktop app that drives all of it. There is no video yet.
The full plan, requirements and progress live in [docs/SPEC.md](docs/SPEC.md).

## Layout

```
crates/
  tether-proto   wire types and codec (shared by every client)
  tether-core    identity, trust store, pairing, Noise secure channel, sessions
  tether-cli     the `tether` command, a terminal front end for the same features
  tether-node    Node and Electron bridge to tether-core (napi-rs)
apps/
  desktop        the Electron app
docs/SPEC.md     specification, security model, milestones, decision log
```

`tether-core` has no platform specific code. Later steps add host agent and client crates
next to these.

## Build and test

Needs Rust 1.80 or newer (`rustup` on Windows and macOS). No other system dependencies.

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
allowed to do. The same data directory is used as by the `tether` command, so the two
interoperate.

Checks that need no display on Windows and macOS, and `xvfb-run -a` on Linux:

```
npm run smoke      # the window opens and shows this device's identity
npm run e2e        # drives the real UI through pair, connect, live grant and revoke
cd ../../crates/tether-node && npm test   # the bridge on its own, no Electron
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

Set `RUST_LOG=debug` for more detail.

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
`crates/tether-core/src/paths.rs`, and the `tether/2` prologue in `crates/tether-proto`
(changing the prologue breaks compatibility with older builds, which is fine before release).
The prologue is `tether/<protocol version>`, so keep the two in step.
