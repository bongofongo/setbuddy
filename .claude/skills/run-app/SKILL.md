---
name: run-app
description: Build and launch Setwave.app so a GUI change can be seen working. Use when asked to run, start, relaunch, or screenshot the app.
---

1. `scripts/run-mac-app.sh` (debug). It rebuilds bindings, the Swift package, assembles
   `target/Setwave.app`, kills the old instance, and opens the new one. Run it in a tmux
   pane so its output stays readable.
2. The app is menu-bar only (`LSUIElement`): no Dock icon. Look for the waveform icon in
   the menu bar. Click it to open the panel.
3. Logs: `log stream --predicate 'process == "Setwave"' --level debug` in another pane.
4. Screenshot the panel with `screencapture -x target/panel.png` after opening it; read
   the PNG to confirm the change.
5. To exercise playback without clicking, drive the same core from the CLI while the app
   is open: `cargo run -q -p setwave-cli -- play crates/setwave-mpv/tests/assets/tiny.webm`.
   They share the state dir and adopt the same mpv.

Stop the app with `pkill -x Setwave`. Never `pkill mpv` blindly; the user may have their
own mpv running.
