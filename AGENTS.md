# Rules for agents in this repository

## Builds and tests

Several agents share one machine. A build or a test run can fill the disk and stop the machine. Obey these rules.

- Run every heavy command through the queue: `umask 022 && sh tools/heavy.sh <command>`. Heavy commands are cargo `build`, `check`, `clippy`, `test`, `nextest`, `install` and `run`, `make check`, and bun `test`, `e2e` and `build`. The queue runs one heavy command at a time on the whole machine, and it refuses to start when the disk is almost full.
- On the owner's Linux machine, a hook blocks a heavy command that does not use this exact path: `sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. Use it there, also in an older worktree.
- Check and test one crate: `cargo check -p <crate>`, `cargo nextest run -p <crate> -E 'test(<name>)'`. Do not build or test the whole workspace unless your task tells you to. The coordinator runs `make check`.
- Do not use `--release`, `--all-features` or a plain `cargo clean`. (`target` can be a link to another disk: `cargo clean` removes only the link.) `cargo clean -p <crate>` is permitted (see the `kache` rule).
- When the queue prints "waiting for a free slot" or cargo prints "Blocking waiting for file lock", wait. Never stop or kill another build.
- Do not run the tests again after a change that only `cargo fmt` made.
- Do not run a test in a loop to reproduce a failure that comes and goes. Report it and ask.
- If cargo reports E0599 for an item that exists, the `kache` compiler cache gave a stale build: run `cargo clean -p <crate>` and build once with `env CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER= cargo …`.
- The rust-analyzer plugin is off in this project. Search the code with CodeGraph or text search.
