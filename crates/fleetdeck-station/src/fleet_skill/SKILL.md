---
name: fleet
description: Use when a task needs another machine or platform (macOS, Omarchy, Linux), more hands, or a second opinion from another session in this Fleet Deck fleet.
---

# Working across the fleet

This fleet has several stations (machines). Each can run Claude and Codex sessions.

1. Call `fleet_view` with `view: "stations"`. It lists every station, its platform (`macos`, `omarchy`, `linux`), whether it is online and its providers. `view: "projects"` (with `station`) lists the projects. Pass `next_cursor` back as `cursor` for more.
2. Call `fleet_view` with `view: "me"` to see your grant: its classes, its stations, its session limit and the highest mode of each provider. A session that the owner starts from the board usually has a small grant that can start a few sessions. Without a grant you cannot start or drive sessions: ask your coordinator with `ask_coordinator`, or tell the owner what you need and on which station.
3. If your tool list has `start_in_project` and the repository is a project on that station: start a session there with `start_in_project`. Otherwise, if your tool list has `spawn`: start a session there with `spawn`. Give it a mode that is not higher than your grant allows. A session that you start gets no grant of its own: it cannot start more sessions.
4. If your tool list has `assign`: give the session one small, testable task and say what to report.
5. If your tool list has `wait_tasks`: wait for the task. A task in `question` needs your answer: if your tool list has `send_mail`, answer with it; otherwise tell the owner.
6. Read the result: the worker publishes a report with `publish_artifact`. If your tool list has `read_artifact`, read it; otherwise ask for a summary in the task result.
7. If your tool list has `stop`: stop the sessions that you started when they are done.

The owner can give, change or end your grant while you run. Then your tool list changes. If a tool that you need is missing, call `fleet_view` with `view: "me"` again before you ask for it.

Rules: never start a session in a bypass mode; never ask for, print or pass on a credential; a refused call is final, so tell the owner instead of retrying.
