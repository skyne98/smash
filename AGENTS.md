# AGENTS.md

Rules for anyone (human or model) changing smash.

## Motion: every state change animates

Every change the user can see is followed by a fast, clear, physics-based
animation, identical in the terminal and the window. No visible value jumps.

- **One system:** `crates/ui/src/motion.rs`. Springs are solved in closed form,
  so they behave the same at any frame rate. Both clients render what
  `Chat::frame` returns. Animate there, never in a client.
- **State stays instant.** Logic, tests and the agent never wait for an
  animation. The view derives each shown value from its target once per frame:
  `motions.tick(now)` first, then `follow(key, target, spring)` for values that
  glide, `start(key, from, to, spring)` for entrances, and `nudge(key, by)` when
  content jumps but must stay put on screen.
- **Interruptible:** a new target mid-motion retargets the spring and keeps its
  velocity. Never restart from zero, never queue animations.
- **Fast and clear:** use the presets. `GLIDE` for movement (scroll, sizes),
  `FADE` for things appearing, `POP` for popups (small overshoot), `COUNT` for
  numbers. Each reaches 90 % in under 120 ms and rests in under about 300 ms
  (`COUNT` under 600 ms). Add a preset only with a test that holds it to the
  same bounds.
- **Grid and pixels:** the terminal rounds positions to cells and fades colour
  toward the background. The window also gets sub-row offsets
  (`Frame::shift`/`above`) and the popup's `open` value.
- **Idle is free:** when nothing moves, `Chat::animating()` is false and clients
  drop to the slow tick (`frame_interval`).
- **Accessibility:** `SMASH_REDUCED_MOTION=1` or `NO_MOTION=1` snaps everything.
  The end state must always be readable without motion.
- **Tested:** a new animated state gets a test in `chat.rs` `motion_tests`,
  driven through `Chat::frame_at` on a fake clock. It must check that the first
  frame starts at the old state, that the motion is continuous or monotonic,
  that it settles within its bound, and that it ends exactly at the new state.

Animated today: transcript scroll (glide, keeps its place when content arrives
while scrolled up), new rows (fade in), session switch and relayout (fade in),
popup (unfolds with a pop, folds away), picker highlight (fades onto the item),
notices (fade in), ctx meter (counts), prompt box height (grows or shrinks a row
at a time), scrollbar thumb (follows the shown scroll), image previews (fade
in with their rows; real pictures follow the same fade and smooth scroll).

## Other rules

- Keep `smash-core` pure: no I/O, no pixels.
- A feature is one Action in core, its Effect handler in backend, and one arm
  per frontend.
- Run `cargo test --workspace` (gpui: `scripts/dev.sh cargo test`) and
  `cargo fmt --all` before committing.
