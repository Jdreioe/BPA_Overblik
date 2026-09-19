# Desktop validation

Issue #2 uses the same package check on Windows, macOS and Linux. The CI matrix
builds a standalone Python worker, places it beside the Iced executable, and
runs `teamup-shift-sync-gui --self-check` without a Python command on the
worker path. The check performs the protocol handshake, loads home status, and
builds the representative fixture preview. The resulting portable directory is
uploaded as a zip artifact.

Run the same check locally with:

```bash
python -m pip install -e '.[package]'
python scripts/package_desktop.py
```

## Accessibility check

The shell uses native Iced buttons with visible text, 12 to 14 pixels of
padding, text descriptions alongside every status, and no color-only meaning.
Loading, empty, failure, and worker-recovery states are part of the Rust widget
tests. Before closing issue #2, record one manual pass for each packaged OS:

- Tab and Shift+Tab reach the week controls, primary action, and retry action
  in reading order. Enter and Space activate the focused button.
- The platform screen reader announces the window title, button text, week,
  service status, preview summary, and errors.
- Focus remains visible at 100% and 200% display scaling, with the content
  reachable by scrolling at a small window size.

Known limitation: Iced 0.14 exposes button text and focus through its platform
accessibility integration, but this first shell has no explicit heading or
live-region semantics. Progress announcements belong to the transfer screen in
issue #6.
