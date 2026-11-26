# hyprhand

A remote control for your [Hyprland](https://hyprland.org) desktop that you use from your phone.
It's one Rust binary that serves an installable web app (PWA). Open it on your phone and you get a
trackpad, keyboard, media controls, workspace switching, file drop and more for the machine it runs on.

## Features

- **Trackpad and keyboard**: relative mouse, clicks, vertical and horizontal scrolling, and text
  typing through a virtual uinput device. Two-finger gestures go to a virtual multitouch touchpad,
  so libinput sees real pinch gestures.
- **Hyprland control**: switch workspaces, focus windows, move windows between workspaces and
  toggle DPMS. Live state comes from Hyprland's event socket.
- **Audio and media**: volume and mute for the default output and mic, switching output and input
  devices, and MPRIS playback control (play/pause, skip, seek, album art) for every player.
- **Brightness**: laptop backlight through `brightnessctl` and external monitors through DDC/CI
  (`ddcutil`).
- **Commands**: a grid of shell commands you define and run with one tap. You can add, edit,
  reorder and delete them from the app.
- **Modes**: scenes made of steps that launch apps on chosen workspaces, close or move windows by
  match, and focus a workspace.
- **Context strip**: per-app shortcut buttons that show up when a given app is open or focused.
- **Files and clipboard**: take a screenshot of any monitor, grab a frame from a webcam, upload files
  to `~/hyprhand-drop`, and read or write the desktop clipboard. Android's share sheet can send
  links, text and files straight to the PC.
- **System**: CPU, RAM, GPU (NVIDIA) and process telemetry, kill a process, and lock the screen
  with `hyprlock`.
- **Fleet**: a list of your other hyprhand machines with their online status from Tailscale.
- **Themes**: `green`, `amber` and `ice` profiles, or `auto` to follow a
  [matugen](https://github.com/InioX/matugen) palette.
- **Login**: PIN login stored as an argon2 hash, JWT session cookies, and lockout after repeated
  failed attempts.

## Requirements

- Linux running Hyprland (the server talks to Hyprland's IPC sockets directly)
- Access to `/dev/uinput`, needed for the trackpad and keyboard

Optional tools; each one enables the feature next to it:

| Tool | Feature |
| --- | --- |
| `pactl`, `wpctl` (PipeWire / PulseAudio) | Volume, mute, device switching |
| `playerctl` | Media controls |
| `brightnessctl`, `ddcutil` | Brightness |
| `grim` | Screenshots |
| `ffmpeg` | Webcam frames |
| `wl-clipboard` | Clipboard and sharing |
| `hyprlock` | Lock screen |
| `tailscale` | Remote access and fleet status |
| `nvidia-smi` | GPU telemetry |
| `xdg-open` | Opening shared links |

On Arch:

```sh
sudo pacman -S --needed pipewire-pulse wireplumber playerctl brightnessctl ddcutil \
  grim ffmpeg wl-clipboard hyprlock
```

## Setup

### 1. Install

Download the archive for your architecture from the
[latest release](https://github.com/rd6260/hyprhand/releases/latest):

```sh
tar xzf hyprhand-v*-x86_64-unknown-linux-gnu.tar.gz
install -Dm755 hyprhand-*/hyprhand ~/.local/bin/hyprhand
```

Or build it from source:

```sh
git clone https://github.com/rd6260/hyprhand
cd hyprhand
cargo install --path server
```

### 2. Allow uinput access

```sh
echo 'KERNEL=="uinput", GROUP="input", MODE="0660"' | sudo tee /etc/udev/rules.d/99-uinput.rules
echo uinput | sudo tee /etc/modules-load.d/uinput.conf
sudo usermod -aG input "$USER"
sudo modprobe uinput && sudo udevadm control --reload && sudo udevadm trigger
```

Log out and back in so the group change takes effect.

### 3. Set a PIN

```sh
hyprhand set-pin 1234
```

### 4. Run it

```sh
hyprhand
```

The first run creates `~/.config/hyprhand/hyprhand.json`. The server listens on
`127.0.0.1:8765` by default. Your phone can't reach that address, so either set `server.bind` to a
LAN or Tailscale IP (see [Configuration](#configuration)) or put a reverse proxy in front of it.

To start it with Hyprland, add this to `hyprland.conf`:

```ini
exec-once = hyprhand
```

### 5. Connect from your phone

Open `http://<pc-address>:8765`, log in with your PIN, then choose **Add to Home Screen** / **Install app**
for the full-screen app.

> Browsers only allow installing a PWA and the share target over HTTPS (or localhost).
> [Tailscale Serve](https://tailscale.com/kb/1312/serve) is the easiest way to get HTTPS:
> `tailscale serve --bg 8765`.

## Configuration

Everything lives in `~/.config/hyprhand/hyprhand.json`. You can also edit it from the **Config**
panel in the app. Missing fields use their defaults, and each save keeps the previous version as
`hyprhand.json.bak`.

```jsonc
{
  "server": { "bind": "127.0.0.1", "port": 8765, "web_dir": null },
  "auth":   { "session_hours": 168, "max_attempts": 5, "lockout_base_seconds": 30 },
  "theme":  { "profile": "green", "matugen_palette": null },

  "commands": [
    { "label": "Suspend", "cmd": "systemctl suspend" }
  ],

  "modes": [
    {
      "name": "Work",
      "steps": [
        { "type": "launch", "cmd": "firefox", "workspace": 1, "match": "firefox", "once": true },
        { "type": "launch", "cmd": "kitty", "workspace": 2 },
        { "type": "close", "match": "discord" },
        { "type": "focus", "workspace": 1 }
      ]
    }
  ],

  "context": {
    "apps": {
      "mpv": {
        "label": "mpv",
        "show": "focus",
        "controls": [
          { "label": "Fullscreen", "key": "f" },
          { "label": "Subs", "key": "v" }
        ]
      }
    }
  },

  "fleet": {
    "hosts": [{ "name": "laptop", "label": "Laptop", "url": "https://laptop.tailnet.ts.net" }]
  }
}
```

`web_dir` serves the UI from a folder on disk instead of the copy built into the binary, which is
handy when working on `web/`. Logging level is set with `RUST_LOG`, e.g. `RUST_LOG=hyprhand=debug`.

## Security

Anyone with the PIN gets full keyboard and mouse control and can run any command you've configured.
Keep it on `127.0.0.1` or a private network such as Tailscale, and don't expose it to the internet.

## Releases

Pushing a `v*` tag runs `.github/workflows/release.yml`. It builds x86_64 and aarch64 Linux
binaries and publishes them to a GitHub release.
