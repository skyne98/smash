# smash

```
smash-tui   smash-gpui        frontends: paint View, map input to Action
     \       /
    smash-client              runtime: state + hooks + effect dispatch
          |
    smash-core                state machine, Action/Effect/Event/View. no I/O, no pixels
          |
    smash-backend             engine: executes effects (files today, daemon later)
          |
    smash-ext                 wasm plugins on every action and every frame
```

| crate | role |
|---|---|
| smash-core | domain, App reducer, view model. Pure. |
| smash-client | Client: owns App, runs hooks, forwards effects to a Backend. |
| smash-backend | the Backend: file store in ~/.local/share/smash. |
| smash-ext | extism host; Hooks impl that chains wasm plugins. |
| smash-session | append-only JSONL log; `Hooks` impl that records every action and event. |
| smash-tui | ratatui binary. |
| smash-gpui | gpui binary (zeron fork: zeronsh/zui). |

A frontend only does two things: client.view() -> paint, input -> client.dispatch(action).
New feature = one Action in core, its Effect handler in backend, one arm per frontend paint.

## Extensions

Drop *.wasm into $XDG_DATA_HOME/smash/extensions (or ~/.local/share/smash/extensions).
Both hooks are optional, JSON in / JSON out, chained in load order:

- smash_action(Action) -> Action: inspect or rewrite any action. Empty reply keeps it, null drops it.
- smash_view(View) -> View: decorate every frame: rows, status, title.

## Run

```sh
cargo run -p smash-tui
cargo run -p smash-gpui      # NixOS: scripts/dev.sh cargo run -p smash-gpui
```

`scripts/dev.sh` is NixOS-only: it puts fontconfig/freetype/xkbcommon on
`PKG_CONFIG_PATH` and `LD_LIBRARY_PATH`, which gpui needs to build and launch.
The terminal client needs no system libs.
