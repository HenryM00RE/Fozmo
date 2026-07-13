# Installing Fozmo on macOS

Fozmo 0.0.1 is packaged for Apple-silicon Macs with macOS 13 or later. The DMG
includes the server, web interface, audio helpers and optional command-line
client, so installing the release does not require Rust, Node.js or FFmpeg.
Hardware, receiver and service compatibility is narrower than the platform
baseline; tested combinations and remaining coverage are recorded in
[platform-support.md](platform-support.md) and
[manual-smoke-tests.md](manual-smoke-tests.md).

This release is not signed with an Apple Developer ID and is not notarized.
macOS will therefore block the first launch until you approve it manually.
Download Fozmo only from the project's official release page and compare the
DMG checksum with the checksum published alongside that release.

## Install the application

1. Download the Fozmo 0.0.1 Apple-silicon `.dmg` from the project release page.
2. Open the downloaded DMG.
3. Drag `Fozmo.app` onto the **Applications** shortcut.
4. Eject the Fozmo disk image.
5. Open **Applications** and double-click **Fozmo**.

The first attempt will normally show a message that macOS cannot verify the
developer. Close that message, then:

1. Open **System Settings**.
2. Select **Privacy & Security**.
3. Scroll to the **Security** section.
4. Find the message saying that Fozmo was blocked and click **Open Anyway**.
5. Authenticate with your Mac password or Touch ID when prompted.
6. Confirm by clicking **Open**.

You should only need to approve a particular build once. Fozmo then appears in
the macOS menu bar and opens its browser interface. The menu-bar controls can
open Fozmo, start or stop the server, show addresses, and open its data or log
folders.

Replacing `Fozmo.app` with a newer version does not remove your library,
metadata, artwork, settings or listening history. Those are stored separately
under `~/Library/Application Support/Fozmo/`.

## Optional command-line control

The DMG contains `fozmoctl`, which can control a running Fozmo server from the
Terminal or from an agent. To make it available as a normal command, run:

```sh
sudo mkdir -p /usr/local/bin
sudo ln -sf \
  /Applications/Fozmo.app/Contents/Helpers/fozmoctl \
  /usr/local/bin/fozmoctl
```

With Fozmo running, check the connection:

```sh
fozmoctl doctor
fozmoctl status
fozmoctl zones list
```

If you want an agent to control Fozmo, give it the
[Fozmo DJ skill](../.agents/skills/fozmo-dj/SKILL.md) and ask the agent to
install the skill itself. Different agent harnesses discover skills in
different locations, so this is more reliable than prescribing one universal
copy command. The agent must be permitted to execute `fozmoctl` locally.

## Removing Fozmo

Quit Fozmo from its menu-bar icon, then remove `Fozmo.app` from Applications.
Removing the app does not automatically delete your music data or settings.
If you also want to remove those, delete
`~/Library/Application Support/Fozmo/` after making any backup you need.
