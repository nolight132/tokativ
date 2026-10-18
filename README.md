<div align="center">

# Tokativ

Local dictation for Linux, macOS and Windows. Press a key, talk, and the text is typed into the
focused app as you speak.

[![License](https://img.shields.io/badge/license-GPL--3.0-2b2b33?style=flat-square)](COPYING)
[![Rust](https://img.shields.io/badge/rust-1.98-2b2b33?style=flat-square&logo=rust)](rust-toolchain.toml)
[![Wayland](https://img.shields.io/badge/wayland-%E2%99%A1-2b2b33?style=flat-square&logo=wayland)](#install)
[![Nix](https://img.shields.io/badge/nix-flake-2b2b33?style=flat-square&logo=nixos)](#install)
[![Platforms](https://img.shields.io/badge/runs%20on-linux%20%7C%20macos%20%7C%20windows-2b2b33?style=flat-square)](#install)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/hero-dark.png">
  <img src="docs/hero-light.png" alt="The Tokativ pill and live caption while dictating" width="900">
</picture>
<br>
<br>

</div>

> [!NOTE]
> The project is vibe-coded as hell.

Speech is recognized on your machine by [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp),
on the graphics card through Vulkan, or Metal on macOS, or on the CPU. Use any model available on Huggingface
within Tokativ's settings.

## Install

On every platform, the first start opens a window that walks through the rest.

### Linux

```sh
nix run github:nolight132/tokativ
```

With Home Manager, which also runs it as a user service:

```nix
imports = [ inputs.tokativ.homeModules.default ];
services.tokativ.enable = true;
```

Both install the released binary; `nix run github:nolight132/tokativ#tokativ-source` builds from
source.

On Arch Linux, install [`tokativ-bin`](https://aur.archlinux.org/packages/tokativ-bin) from the
AUR:

```sh
yay -S tokativ-bin
```

Elsewhere, download the binary from the
[latest release](https://github.com/nolight132/tokativ/releases/latest), make it executable and
put it on your `PATH` as `tokativ`. It needs glibc 2.35+, the Vulkan loader, Wayland, libxkbcommon
and alsa-lib.

Tokativ needs a compositor with layer shell, such as niri, Hyprland, Sway, river or KDE Plasma.
GNOME does not work.

### macOS

Download the `.app.tar.gz` for Intel or Apple silicon from the
[latest release](https://github.com/nolight132/tokativ/releases/latest), extract it, and move
Tokativ to Applications. Open it from the context menu on first launch; builds are not notarized.

On Apple silicon, the Nix commands above build from source, and Home Manager runs Tokativ as a
launchd agent.

### Windows

Run the `-setup.exe` installer for your architecture from the
[latest release](https://github.com/nolight132/tokativ/releases/latest). A portable `.exe` is
also there.

## Build from source

Install Rust through rustup (the checkout selects its version), CMake and a C++ toolchain:

- **Windows:** Visual Studio Build Tools with Desktop development with C++, a Windows SDK,
  NASM and the Vulkan SDK. Use Developer PowerShell; for ARM64, use clang-cl and Ninja.
- **macOS:** Xcode command line tools.
- **Linux:** pkg-config, glslc and development packages for Vulkan, SPIRV-Headers, Wayland,
  libxkbcommon, fontconfig, alsa-lib and OpenSSL. `nix develop` provides them.

```sh
git clone https://github.com/nolight132/tokativ.git
cd tokativ
cargo build --release --locked
```

Run `target/release/tokativ` (Windows: `.\target\release\tokativ.exe`).

## License

GPL-3.0-or-later.
