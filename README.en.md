<p align="center">
  <img src="docs/poster-en.svg" alt="RCTool: turn a Xiaomi voice remote into a desktop microphone" width="100%">
</p>

<p align="center"><a href="README.md">中文</a> | English</p>

**Currently supported:**

- Xiaomi Bluetooth Remote v2 (RC001)
- Xiaomi Bluetooth Remote v2 Pro (RC003)

There are many remotes out there — PRs adding support for more models are welcome.

## Quick Start (macOS)

1. Pair the remote in System Settings → Bluetooth: hold **Home + Menu** to enter pairing mode; it shows up as `MI RC`
2. Launch the app: `cargo run -p rctool-tray`, or grab the dmg from Releases
3. On the Connection page pick **BlackHole 2ch** as the output device (the app offers one-click install if missing; the full dmg ships the installer built in)
4. System Settings → Keyboard → Dictation: enable it, set the shortcut to **hold 🌐**, and select BlackHole 2ch as the microphone source
5. Hold the remote's mic button and speak; release to finish

Key remapping (optional): open the Keys page, click a button on the remote diagram to remap it. Requires Input Monitoring and Accessibility permissions.

Want one app to behave differently (say OK sends Space inside a video player)? Add an override layer for it on the Apps page. You only write the keys that differ from the global map, and it kicks in whenever that app is in front.

## Keeping the Mac Awake

While the remote is in Bluetooth range, RCTool can keep the Mac out of idle sleep and unlock
a locked screen. An always-on Mac mini stays reachable over SSH and Screen Sharing.

On the Device page:

1. Scan and bind your remote. Once bound, both the voice bridge and presence detection accept
   only that one, so an identical remote nearby never gets picked up by mistake
2. Turn on **Keep Awake While Device Is Nearby**. It holds a `PreventUserIdleSystemSleep`
   assertion, visible in `pmset -g assertions`. Choosing Sleep from the menu or pressing the
   power button still puts the Mac to sleep
3. To skip the password too, store your login password once (it goes to the Keychain) and turn
   on **Unlock on Any Screen Lock**. Requires Accessibility permission
4. On a headless machine, also turn on **Launch at Login** on the Connection page, or none of
   this survives a reboot

Auto-unlock is not Apple's Auto Unlock. The password goes in through synthetic keyboard events,
so anyone who can spoof your remote can unlock the Mac. It ships off and asks for confirmation
when you turn it on.

Presence comes from the system Bluetooth connection rather than signal strength, so the
resolution is connected or not connected. A remote sitting on the desk counts as present.

## Usage

```bash
# GUI: main window + menu bar tray
cargo run -p rctool-tray

# CLI
cargo run -p rctool-cli --release -- outputs            # list audio output devices
cargo run -p rctool-cli --release -- scan               # find the remote
cargo run -p rctool-cli --release -- run --output BlackHole   # run the bridge
cargo run -p rctool-cli --release -- run --wav test.wav       # debug: dump voice to wav
```

## Platforms

| Platform | Voice → virtual mic | Dictation trigger | Key remapping | Keep awake / auto-unlock |
| --- | --- | --- | --- | --- |
| macOS | ✅ BlackHole (bundled installer in full edition / brew·web guidance in lite) | Hold mic button to dictate (device-scoped F5→Fn remap) | ✅ 13-key visual remapping + per-app overrides | ✅ Power assertion + lock-screen typing + launch at login |
| Windows | ✅ VB-Cable (one-click fetch from the official source) | Auto Win+H on voice | Not yet | Not yet |
| Linux | ✅ One-click null-sink, nothing to download | No system dictation; virtual mic only | Not yet | Not yet |

## Development

```bash
just --list                      # all commands (install just: brew install just)
just run                         # run the GUI (static frontend, no node needed)
just ci                          # same three steps as CI: test + full build + clippy
just install                     # bundle and install into /Applications
just deploy minits               # bundle and deploy to a remote Mac
just dist                        # everything into dist/ (macOS: full/lite dmg + app.zip + CLI)
git tag v0.1.0 && git push --tags  # CI builds installers for all 3 platforms (draft Release)
```

Sign with a certificate on macOS where you can (`just sign-id` picks the only local one
automatically). An ad-hoc signature's designated requirement is the binary hash, which changes
on every rebuild, so system permissions have to be granted again after each reinstall.

More docs: [apps/tray/README.md](apps/tray/README.md) (architecture, per-platform differences, packaging),
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) (BlackHole bundling license notes).

## License

GPL-3.0-only. Protocol details and decoder logic derive from [nijez/open-voice-bridge](https://github.com/nijez/open-voice-bridge) (GPL-3.0).
