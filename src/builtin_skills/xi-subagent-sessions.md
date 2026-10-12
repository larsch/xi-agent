# Working with separate xi sessions

This skill covers the mechanics of running separate xi sessions and contacting them over session IPC. Follow the user's workflow for whether to delegate, what work to assign, how to review results, and other lifecycle decisions.

## Check the environment before launching

Use a launcher that is available in the current environment or specified by the user. Do not assume a launcher from the operating system alone.

Check relevant environment clues and verify tools before relying on them. For example, `TMUX` may indicate that this xi session is running inside tmux. On Unix-like systems, check whether tmux is available before using it. Treat clues as things to verify, not guarantees.

When using tmux, start the worker in a **new window or pane**; do not replace or take over the current pane. Record the window or pane created so it can be stopped later. Tmux guidance applies only when tmux is available; do not present it as cross-platform instructions.

If no suitable launcher is evident, use the user's preferred launcher or ask which one to use. Launcher choices may include a terminal window or tab, tmux, a service manager, or another mechanism available to the user.

## Start a worker session

Use a separate worktree for each worker session. Start xi with its working directory set to that worktree and enable session IPC:

```text
xi --enable-session-ipc
```

Adapt the command and working-directory setup to the chosen launcher. Starting a process in another terminal, tab, pane, or service is launcher-specific; do not assume one command works everywhere.

Session IPC supports Unix sockets and native Windows named pipes, restricted to the current user. Windows and WSL sessions are separate; there is no automatic bridge. On Windows, verify `wt.exe` and the xi binary before opening a new Windows Terminal tab with its starting directory set to the worker workspace.

## Connect and interact

Use the `agent_session` tool with the worker worktree's **absolute path** as `cwd`. The tool handles the IPC connection; do not try to locate or connect to an IPC endpoint directly.

- `inspect` identifies the session and reports its capabilities.
- `state` reports whether it is idle or working.
- `post_prompt` submits a prompt.

A successful `post_prompt` response reports whether the prompt was accepted and its `turn_id`; it is not the worker's final answer. Completion or control-revocation events identify the same turn. Steering input belongs to the existing active turn. A response timeout does not prove the prompt was rejected: inspect the session before deciding what to do, and do not blindly resubmit it.

If the session is unavailable, check that the worktree path is correct, the worker is running there, session IPC was enabled, and the platform supports IPC. Do not infer that the worker failed or start duplicate workers without checking.

## Stop and clean up

IPC currently has no operation to terminate a worker. Stop it through the same launcher used to start it—for example, close the specific tmux window created for the worker, or stop the specific service created for it.

Identify the target before stopping anything. Do not terminate an unrelated xi process, take over the user's active terminal, or remove a worktree that contains user work. Clean up only resources created for this worker, and follow the user's workflow about whether the worktree should be retained.
