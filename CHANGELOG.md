# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- A choice of what the dictation key does, under Settings → Dictation → Shortcut: tap or hold
  as before, toggle, where every press starts or finishes, or push to talk, where letting go
  always finishes. How long a press lasts before it counts as holding can be set too. On Plasma
  the section shows the key and opens System Settings to change it.

### Changed

- The tray icon on macOS and Windows is a microphone, and a red dot while you dictate. On macOS
  it is sharp on Retina displays and sized like other menu bar icons.

### Fixed

- Plasma shortcuts with several keys showed a literal `\t` between them.

## [0.5.0] - 2026-10-19

### Added

- Push to talk. Hold the dictation key while you talk and let go to finish, or tap it to start
  and tap again to finish as before. It works on macOS, Windows, Hyprland, Sway, river, niri and
  Plasma. Setup shows the new bindings, which run `tokativ press` and `tokativ release`, or
  `tokativ hold` on niri, which cannot run a command when a key comes up. On Plasma Tokativ
  registers a Dictate shortcut of its own, on Meta+Shift+D unless another app has it.
- The input method on KDE Plasma. KWin keeps it for the program chosen under System Settings →
  Keyboard → Virtual Keyboard, so Tokativ offers itself there and picks itself once when nothing
  else is chosen. Text, live drafts, and Escape and Enter during a session then work in Plasma
  apps as they do on niri.
- An Appearance page with a live preview of the pill and its caption, edited in place. Clicking
  a button or the caption hides it, dragging an end of the pill or the caption sets its width,
  and holding the arrow under the pill drops it onto a drawing of the main monitor to set its
  edge and its distance from it. A Visual and List switch brings back the plain rows.
- Three layouts for the pill's bars: low to high tones, mirrored from the middle, or a peak in
  the middle. The preview picks one from its corner.
- Settings for the pill's width and for how wide the live caption grows.
- Rosé Pine, Nord and Dracula themes, shown with the others as cards that scroll sideways.
- A model's page with its model card, likes and the file recommended for the device, opened
  from the model search or from the info button beside an installed model.
- Each model in the lists shows the first line of its model card. The cards are kept on disk,
  so the lines are there as soon as a list opens.
- The setup asks whether the graphics card or the processor runs the model before offering
  models that suit it, with a warning when one may not fit in its memory.
- Links to the Open ASR Leaderboard and Handy's model list beside the model search.
- Ctrl+F, or Cmd+F on macOS, puts the cursor in the settings search.

### Changed

- The theme and the pill's settings moved from General to Appearance.
- The resources show as three readings side by side, each with a bar of how much of the whole
  it takes.
- Models are named after their repository rather than a written summary, and a model's page
  opens Hugging Face in the browser instead of copying its link.
- The model list shows models while they download, and keeps a failed download there to try
  again.
- The live caption grows to at most 550 px, and the settings window keeps a width that shows
  the widest caption whole.

### Fixed

- The niri and Sway bindings from setup no longer start and finish dictation over and over
  while the key is held. The niri binding no longer needs a cooldown.
- On Plasma, text the input method cannot reach, as in X11 apps, is pasted with KWin's fake
  input. It used to be left on the clipboard.
- On Plasma the pill opens on the active output, and Shift and Ctrl reach the app while the
  input method holds the keyboard.
- "Drop the last" period only drops the one that ends the dictation. Pausing between sentences
  used to lose all their periods.
- Device names leave out the driver and codename Vulkan appends.
- The launch at login entry keeps pointing at the installed binary.
- The tray icon shows up when the panel starts after the daemon.
- macOS keeps the Accessibility grant across updates.

## [0.4.0] - 2026-10-18

### Added

- Writing profiles that decide capitals, periods, laughter, spoken emoji and replacements.
  Formal and Casual come built in, more can be added, renamed and removed under Writing style,
  and the tray or `tokativ profile <name>` switches between them. A writing style from an
  earlier version becomes a profile of its own.
- A theme picker with System, Dark and Light, and custom themes read from JSON files in the
  themes folder in the format Sonora reads.
- Switches that hide the pill's cancel and confirm buttons.

### Changed

- Settings follow Sonora's layout: a search over every setting, a bar of categories under it
  that blurs the rows scrolling beneath, and an entrance when the category changes. General comes
  first, the model search opens from the Model category, and Runtime is a setting of its own.
- The first-run setup runs inside the settings window and nothing else opens until it is done.
- The settings list only the installed models. The setup offers Parakeet for the CPU and Whisper
  large v3 turbo for a graphics card.
- Rounded corners across the app are squircles, the live caption included, and the caption
  blurs as it folds away.
- The window is called Tokativ, and on Windows it draws its minimize, maximize and close
  buttons.

### Fixed

- Whisper keeps writing in the language spoken when the speech is fast, and no longer answers
  with its vocabulary prompt, such as "Glossary", when it is unsure.
- Text reaches X11 apps under Xwayland by pasting, since they ignore the input method and read
  typed keys in the wrong layout.
- Recording carries on after the audio device reports a gap in its buffer.

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

[unreleased]: https://github.com/nolight132/tokativ/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/nolight132/tokativ/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/nolight132/tokativ/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/nolight132/tokativ/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/nolight132/tokativ/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/nolight132/tokativ/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/nolight132/tokativ/releases/tag/v0.1.0
