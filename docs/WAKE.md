# Setting up wake from sleep

Waking a computer remotely needs three things to line up: the computer must be set to listen for a wake signal while asleep, something that stays on must send the signal from the same network, and your device must be allowed to ask for it. Menu names below vary between Windows, macOS and BIOS versions, so treat them as a guide to what to look for.

## 1. The computer to be woken

**Use Ethernet.** Wake over Wi-Fi is unreliable on both Windows and macOS. A cable is the dependable way.

**Find its MAC address**, which is what the wake signal is addressed to:

- Windows: `ipconfig /all`, the "Physical Address" of the Ethernet adapter.
- macOS: `ifconfig en0 | grep ether` (the Ethernet port may be `en1` or another number on some Macs).
- Linux: `ip link`.

**Windows**

1. BIOS or UEFI setup: turn on the option for waking on LAN or on a PCI-E device. It is called different things ("Wake on LAN", "Power on by PCI-E", "Resume by LAN"). Turn off any "ErP" or "EuP" power saving option, which cuts power to the network card when the computer is off.
2. Device Manager, your Ethernet adapter, Power Management tab: allow this device to wake the computer, and tick "Only allow a magic packet to wake the computer" so other traffic does not wake it.
3. Same adapter, Advanced tab: make sure "Wake on Magic Packet" is enabled.
4. To wake from shut down rather than sleep, turn off Fast Startup (Control Panel, Power Options, Choose what the power buttons do). Waking from sleep needs none of this.

**macOS**

- A Mac wakes from sleep only, not from shut down. Put it to sleep, do not shut it down.
- System Settings, in the Battery or Energy section: turn on "Wake for network access" (the wording and place differ by macOS version). On a laptop it generally needs to be on power.
- Wi-Fi wake exists on some Macs but is unreliable. Prefer Ethernet.

## 2. The device that stays on

Any computer running Tether can do it, and a Raspberry Pi is the usual choice. It must be on the same network as the computer being woken, because wake signals do not cross routers.

```
tether service install --wake desk=aa:bb:cc:dd:ee:ff@192.168.1.255
```

- `desk` is the name you will use to ask for it. Pick anything.
- The part after `@` is your network's broadcast address. For a home network on `192.168.1.x` it is almost always `192.168.1.255`. If you leave it out, `255.255.255.255` is used, which works on many networks but is blocked on some.
- Repeat `--wake` for more computers.

Then pair your phone or laptop with it and give it `power` (`tether peers grant <device> power`).

## 3. Asking for the wake

- Desktop app: connect to the always-on device, type the name (`desk`), press Wake. The box appears once the device has `power`.
- Command line: `tether wake pi.local desk`.

Away from home, reach the always-on device over Tailscale or similar; the wake signal is then sent from the Pi onto your home network.

## If it does not wake

1. Try it from a computer on the same network first, to rule out reachability. `tether wake` tells you when the relay sent the signal; it cannot tell you whether the computer woke.
2. Check the MAC address, and that the cable is plugged into the port you took the MAC from.
3. Check the broadcast address matches your network.
4. Waking from sleep works far more often than waking from shut down. Test with sleep first.
5. Some network cards and routers drop wake signals. A fixed broadcast address (step 2) and a cable usually fix that.

Nothing here has been tested against a real sleeping computer by the people who wrote it. The relay and the signal format are tested; whether your particular hardware wakes is down to your BIOS and network card.
