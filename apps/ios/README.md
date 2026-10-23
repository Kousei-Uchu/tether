# Tether for iPhone

A controller for your computers: a trackpad, a keyboard and media keys. It pairs with a
computer by scanning the QR code that computer shows, then connects over your local network
(or Tailscale) with the same encrypted, authenticated channel the desktop app uses.

The networking and cryptography are the Rust core, not a Swift rewrite. `crates/tether-ffi`
exposes it through UniFFI, which generates the Swift API the app calls.

**Status: this app has not been compiled or run.** It was written without access to Xcode.
The Rust interface it depends on is tested (from Python, against a real host, with
`scripts/ffi-check.sh`), but expect to fix some Swift compile errors the first time you build.
Send them back and they get fixed.

## Build

You need a Mac with Xcode 15 or newer, Rust (`rustup`) and XcodeGen (`brew install xcodegen`).

```
scripts/build-ios.sh        # from the repository root: builds the Rust library for iPhone
cd apps/ios
xcodegen                    # creates Tether.xcodeproj
open Tether.xcodeproj
```

`build-ios.sh` produces `apps/ios/Frameworks/TetherFFI.xcframework` and the generated
`apps/ios/Tether/Generated/tether_ffi.swift`. Rerun it whenever the Rust interface changes.

In Xcode, pick your team under Signing & Capabilities, choose your phone or a simulator, and
run. The simulator cannot scan QR codes (no camera): use **Type it** there, or paste a link.

## Using it

1. On the computer, open the Tether desktop app, tick **Allow remote control** if you want
   the phone to move the pointer and type, and start accepting connections. It shows a QR
   code.
2. On the phone, tap **Add a computer** and scan it. The first time, iOS asks to allow the
   camera and access to devices on your local network. Allow both.
3. The phone connects. By default a newly paired device may only view. On the computer, give
   the phone more in the **Devices paired with this machine** list: `input` for the trackpad
   and keyboard, `media` for the media keys. The phone's controls enable themselves as soon as
   the permission arrives; no reconnecting.

## Controls

| Gesture | Does |
| --- | --- |
| One finger drag | Moves the pointer. Faster swipes travel further |
| Tap | Left click |
| Two finger tap | Right click |
| Two finger drag | Scrolls (natural direction) |
| Tap, then press and hold, then drag | Drags. The button stays down until you lift |

The **Keyboard** button opens the iOS keyboard. What you type is sent as exact text, so it
comes out right whatever the computer's keyboard layout. Return and Delete are real key
presses. The row of keys above the media controls sends Escape, Tab, arrows and so on; tap
Ctrl, Alt, Cmd/Win or Shift first and the next key is sent with it held, so Ctrl then `c` is
Ctrl+C.

## Things to know

- iOS suspends network connections when the app is in the background, so Tether disconnects
  when you leave it. Tap the computer again to reconnect.
- The phone keeps its identity in the app's Application Support folder. Deleting the app
  forgets it, and every computer will need to pair with the phone again.
- There is no screen view on the phone yet. That comes after screen capture exists on the
  computers.
- A hardware keyboard attached to the phone is not forwarded yet.
- Wake from sleep arrives with the wake relay.

## If the build fails

- *Linker errors mentioning `_res_9_...` or Security*: add `-lresolv` and the Security
  framework to Other Linker Flags.
- *"No such module tether_ffiFFI"*: rerun `scripts/build-ios.sh`, then regenerate the project
  with `xcodegen`, and check `Frameworks/TetherFFI.xcframework` exists.
- *Swift errors in `tether_ffi.swift` itself*: the generated file must come from the same
  build as the library. Rerun `scripts/build-ios.sh`.
