# Oniko (Rust)

Desktop chat for coding agents over ACP, rewritten from Electron + React to Rust on [gpui](https://crates.io/crates/gpui). It sends anonymous telemetry to the same API as the Electron app; `ONIKO_TELEMETRY=0` turns it off.

## Run

```sh
cargo run -p oniko                 # debug build
./scripts/bundle-macos.sh          # release Oniko.app in target/release/bundle
cargo test --workspace             # core and UI unit tests
```

The app opens the same `oniko.db` the Electron app used (`~/Library/Application Support/oniko` on macOS). Up to migration 5 the schemas are identical. Migration 6 adds task models and dependencies, which the Electron app does not know.

## Tasks

Each project has a board of tasks with the statuses Backlog, To do, In progress, In review and Done. A task waits in a "Not started" status until every task it depends on is completed, then it starts at once. There is no limit on how many run in parallel.

- Each started task gets its own git worktree and branch, created from a fresh `origin/HEAD`. Oniko never checks out or pulls in the project folder itself, and runs its own git commands for one project one at a time.
- A task can pin the model its agent runs on. If the agent does not offer that model, the start fails instead of falling back.
- Task agents run in Claude's auto permission mode, so nobody approves each command. On a model without auto mode the adapter falls back to accepting edits and asks for the rest. Oniko notices the fallback, tells you once per task, and remembers it per agent and model. The task page's model menu marks such a model "Asks before commands".
- With a pull request, the agent rebases on the base branch, pushes and runs `gh pr create`. Oniko then reads the pull request's state from GitHub with `gh pr list`, every 30 seconds, while any is open. Run plan turns on the pull request of every task it queues.
- When the pull request is open, the task moves to the first "In review" status. In review is not done: the tasks waiting on it start only after the merge. A project from before this status gets one after its first "In progress" status.
- While a pull request waits, its clean worktree is removed and the branch kept. On a conflict, Oniko restores the worktree and asks the agent to fix it once, then tells you. A prompt you send from the thread also restores it first, and the worktree is removed again when that turn ends.
- On merge, the task moves to Done, its worktree and local branch are deleted, and the tasks waiting on it start from the new base.
- Every task agent gets `CARGO_TARGET_DIR=<project>/target`, so all worktrees of a Rust project share one build folder instead of each growing its own. Parallel builds wait for Cargo's lock on it.
- The tasks header of a Cargo project shows the size of that folder. Its menu runs `cargo clean` in the project, except while an agent of the project may be building.

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
