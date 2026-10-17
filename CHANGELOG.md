# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Writing profiles that decide capitals, periods, laughter, spoken emoji and replacements.
  Formal and Casual come built in, more can be added, renamed and removed under Writing style,
  and the tray or `tokativ profile <name>` switches between them. A writing style from an
  earlier version becomes a profile of its own.

### Changed

- Settings follow Sonora's layout: a search over every setting, a bar of categories under it
  that blurs the rows scrolling beneath, and an entrance when the category changes. General comes
  first, the model search opens from the Model category, and Runtime is a setting of its own.
- The first-run setup runs inside the settings window and nothing else opens until it is done.

## [0.3.1] - 2026-10-17

### Added

- Windows installers through Inno Setup for x86_64 and ARM64, with per-user installation,
  shortcuts and an uninstaller. Portable executables remain available.
- Update checks on Windows and macOS, with a popup offering a verified download and restart.
  Checks can also be started from the tray or settings, and automatic checks can be disabled.
  Notifications wait until dictation finishes. Installations managed by a package manager keep
  their existing update method when they are not macOS app bundles.

### Changed

- Settings now have five pages: typing options live in Dictation, while pill appearance,
  system options and resource usage share General.

## [0.3.0] - 2026-10-17

### Added

- Tokativ runs on macOS and Windows. It listens for a hotkey of its own there, Option+Space on
  macOS and Ctrl+Alt+Space on Windows, and types or pastes the text into the focused app. To
  change the hotkey, click it in settings and press the new one. Function keys work on their own.
- A vocabulary on the Model page lists names and terms to spell as written. Whisper and Qwen3-ASR
  also follow the text before each phrase, both what the field already held and what was said
  before, so names stay spelled the same and phrases join up. Parakeet and GigaAM cannot take
  either, and settings say so.
- The flake builds Tokativ on Apple silicon Macs, and its Home Manager module runs it there as a
  launchd agent.
- Release builds for Linux, Windows and macOS on x86_64 and ARM64. macOS downloads include an
  app bundle with microphone permission metadata.
- An optional floating live caption on every platform, with smooth resizing, scrolling and
  text morphing. New words sharpen from a blur, while outgoing lines fade away.
- Spoken emoji names in over 140 languages, enabled by default in Writing style. Explicit
  phrases such as "thumbs up emoji" become emoji locally, including flags and skin tones.

### Changed

- Settings leave out the descriptions that only repeated a setting's name, and search results
  show how often a model was downloaded instead of its likes.
- Model descriptions summarize each model's purpose without language-specific comparisons.

### Fixed

- Windows and macOS overlays no longer draw the system frame around their transparent surface.
- The pill keeps its neutral background during errors, with a red indicator instead of a red fill.
- Live captions render color emoji, preserve words when punctuation changes, and hide old lines
  completely after scrolling settles. Shadows have enough room on Wayland surfaces.

## [0.2.0] - 2026-10-17

### Changed

- `nix run github:nolight132/tokativ` and the Home Manager module install the released binary
  instead of compiling Tokativ. `nix run github:nolight132/tokativ#tokativ-source` still builds it
  from source.

## [0.1.0] - 2026-10-17

The first release. Bind `tokativ toggle` to a key, press it and talk, and the text appears in the
focused app as you speak.

### Added

- A pill at the bottom of the screen shows that Tokativ is listening, with a level meter fed by
  your microphone. Enter finishes the dictation and Escape cancels it.
- A live draft of each phrase sits at the cursor while you talk, and the finished phrase replaces
  it. Text goes in through the input method, and Tokativ falls back to typing or pasting where
  there is none.
- Speech is recognized on your machine by transcribe.cpp, on the graphics card through Vulkan or
  on the CPU. Any of its GGUF models works, and settings can search Hugging Face for them and
  download them.
- A setup window on the first start downloads the model you pick, Parakeet by recommendation,
  and shows the keybinding line for your compositor. Nothing is downloaded before that.
- Settings let you pick the model and the microphone, and show how much memory, graphics memory
  and processor time the daemon uses.
- A writing style rewrites each phrase before it is inserted. It decides how sentences start,
  whether the final period stays, how laughter is spelled, and which fixed phrases get replaced.
- A tray icon and a `tokativ` command line control the daemon.
- The Home Manager module runs Tokativ as a user service and can fetch models at build time.

[unreleased]: https://github.com/nolight132/tokativ/compare/v0.3.1...HEAD
[0.3.1]: https://github.com/nolight132/tokativ/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/nolight132/tokativ/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/nolight132/tokativ/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/nolight132/tokativ/releases/tag/v0.1.0
