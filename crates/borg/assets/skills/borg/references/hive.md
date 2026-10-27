# Hive: opening a goal for the drones

A hive is a goal several drones work on together. The engineer starts `borg-mesh` and opens sessions
(Claude Code, omp, opencode or codex). Each session that runs `borg hive join` becomes a drone: it
waits for work without spending tokens and is woken when a task is handed to it. One session, the
planning session, turns what the engineer wants built into the goal. That session is often this one.

## When the engineer says what to build

1. **Read enough of the repo to plan.** Know which files each piece touches and which checks prove it
   works. Ask the engineer only when the goal itself is unclear.
2. **Split the work into small, independent tasks.** Each task fits one drone's session. Give each task
   its own files where possible, so two drones never edit the same file.
3. **Put contracts before their dependants.** When tasks share an interface (a type, an API, a file
   format), one task defines it first. Every task that uses it lists that task as a dep. The task that
   defines the interface publishes it with `borg hive contract "<the interface>" --scope <files>`.
4. **Give every task a check.** A check is a shell command that passes only when the task is done. It
   is the task's own oracle, the sixth field of `--task` (`NAME=CMD`, which may contain `|`). Prefer a
   test that names the task's behaviour over "the whole suite passes".
5. **Open the goal**:

   ```sh
   borg hive init <goal> --spec "<what and why, in the engineer's words>" \
     --oracle "all=<the repo's full test command>" --budget <USD> \
     --task "api|work|define the parser's AST|src/ast.rs||ast=cargo test ast::" \
     --task "lexer|work|tokenize the input|src/lexer.rs|api|lexer=cargo test lexer::" \
     --task "parser|work|parse tokens into the AST|src/parser.rs|api,lexer|parser=cargo test parser::"
   ```

   `--task` is `ID|role|title|files,..|deps,..|NAME=CMD`. The role is `work`: borg opens the verify,
   fix and integrate tasks itself, and a verify task opened by hand waits for the approval like any
   other. To add tasks to an open goal, use
   `borg hive open <goal> --id ID --title T --files a,b --deps x --accept NAME=CMD`.
6. **Put the plan to the engineer.** `init` and `open` print the plan, one card per task, and the queen
   question it waits on. Show the engineer the cards and ask them to answer **at their own prompt**:
   `yes` approves the plan, `no, because <why>` refuses it. No drone takes a task before the yes.
   Never answer the question yourself. No command run in a session approves it: `borg queen answer`
   and `borg hive ratify` both refuse.
7. **After the engineer's answer**, the next prompt's context says what happened:
   - **Approved.** Tell the engineer, in one line, what the context says: "N drones on M nodes
     waiting". The drones pick the work up by themselves.
   - **Refused.** The reason is a veto, which borg learns from. Revise the plan with the engineer:
     open the changed or missing tasks with `borg hive open`. The new plan is asked again.

A goal opened from the engineer's own terminal has no session to ask. The engineer approves it there
with `borg hive ratify <goal>`.

## While the drones work

- `borg hive board <goal>` shows the tasks, who holds them, and what is verified and integrated.
- A drone may use subagents or workflows inside its task: they work under its session. They never run
  `borg hive claim`, `borg hive done` or any other `borg hive` drone command. Only the drone's own
  session claims and finishes its task.
- **Budget.** Claude Code sessions are metered from their transcripts. omp, opencode and codex report
  no usage, so their spend shows as unmetered. Once the goal's metered spend reaches `--budget`, the
  drones claim nothing more, and the engineer is asked once: "raise budget of G?". They answer at their
  prompt in the session that opened the goal: the new budget in dollars, or `yes` to double it, or
  `no` to keep it. Or they run `borg hive ratify <goal> --budget USD` in a terminal.
- To stop being a drone, say "leave the hive": `borg hive leave`.

## Commands at a glance

| Who | Command |
|---|---|
| Planning session | `borg hive init <goal> ... --task ...`, `borg hive open <goal> --id ...` |
| Engineer, in their terminal | `borg hive ratify <goal>`, `borg hive ratify <goal> --budget USD`, `borg hive approve <goal>` |
| Any session that will be a drone | `borg hive join` (every open goal of the repo) or `borg hive join <goal>` |
| A drone | `borg hive leave`; claims, dones and verdicts come through its hooks |
