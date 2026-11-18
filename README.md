# Oniko (Rust)

Desktop chat for coding agents over ACP, rewritten from Electron + React to Rust on [gpui](https://crates.io/crates/gpui). The web app (`../oniko-web`) is unchanged; this app still talks to its telemetry API.

## Run

```sh
cargo run -p oniko                 # debug build
./scripts/bundle-macos.sh          # release Oniko.app in target/release/bundle
cargo test --workspace             # core and UI unit tests
```

The app opens the same `oniko.db` the Electron app used (`~/Library/Application Support/oniko` on macOS). The schema, migrations and JSON columns are identical, so either app can read what the other wrote.

## The agent

The client side of ACP is the Rust SDK ([`agent-client-protocol`](https://agentclientprotocol.com/libraries/rust)). Claude's ACP adapter is still a Node package, so Oniko starts it from, in order:

1. `ONIKO_CLAUDE_ACP`, a full command line.
2. `claude-agent-acp` on the PATH (`npm install -g @agentclientprotocol/claude-agent-acp`).
3. `npx --yes @agentclientprotocol/claude-agent-acp@0.98.0`.

Login checks run `claude auth status --json` with `claude` from the PATH, or `ONIKO_CLAUDE_CLI`. The PATH is read once from an interactive login shell, so an app opened from Finder still finds Homebrew, nvm and `~/.local/bin`.

Each agent runs in its own process group, and closing a thread or quitting stops the whole group.

## Environment

| Variable | Effect |
|---|---|
| `ONIKO_USER_DATA` | Folder for `oniko.db` and agent logs |
| `ONIKO_WORKTREES` | Root for task worktrees, default `~/.oniko/worktrees` |
| `ONIKO_CLAUDE_ACP` | Command that starts the Claude adapter |
| `ONIKO_CLAUDE_CLI` | Path of the `claude` CLI for login checks |
| `ONIKO_TELEMETRY=0` | Turns telemetry off |
| `ONIKO_TELEMETRY_URL` | Telemetry server; also settable at build time |
| `ONIKO_SMOKE_CWD` | Adds a folder to recent projects and turns telemetry off |

## Layout

- `crates/oniko-core` has everything except drawing: the ACP session, SQLite, subscriptions, tasks and their git worktrees, telemetry. One thread owns all state, as Electron's main process did. Commands and background results arrive as jobs on that thread. The UI uses `CoreHandle` and the event stream only.
- `crates/oniko` is the gpui app. It applies the same reducer to the event stream that the core runs, as the renderer did.
- `crates/oniko/src/ui` holds the stand-in components (button, menu, modal, text field). Swap them for the component library when it is ready.

gpui is pinned to crates.io `0.2.2` in the workspace `Cargo.toml`. If the component library builds on a different gpui source, point that one line at the same source.

## Not carried over yet

- Syntax highlighting in code blocks (shiki) and most motion animations.
- In the status menu, typing to filter and number shortcuts. In drag and drop, dropping below a task (a drop lands above the task under the pointer).
- The `S` shortcut on the task page, and the remembered list or board layout.
- The Playwright smoke test, which drove Electron. `oniko-core/examples/session_smoke.rs` runs real turns against the adapter instead.
- Telemetry sends no GPU details; the server's `app.electron`, `chrome` and `node` fields are sent empty.

Windows and Linux builds are written for but untested.
