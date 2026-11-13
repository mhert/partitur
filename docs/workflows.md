# Workflow reporting

A session that works through the superpowers skills (brainstorming, writing-plans,
executing-plans, subagent-driven-development, test-driven-development,
finishing-a-development-branch) can tell Partitur where it stands. Partitur shows that as a
flow rail above the transcript, a step column in the tree and a Spec tab that holds the
design and the plan, and it holds the agent at every gate until you decide. A session that
never reports looks as before, except that its Spec tab is disabled.

## The tools

Every session gets Partitur's MCP server `partitur`. A plain session lists only the
`superpowers_*` tools below; an orchestrator lists its [orchestration
tools](orchestration.md) as well. Calls to them are allowed without asking. Partitur adds a
short instruction block to the system prompt of every session when it creates or resumes it;
it tells the agent to call these tools only while it works with the superpowers skills, and to
end its turn after a gate. A path an agent reports is relative to the session's working
folder, which is its worktree when it has one.

- `superpowers_start_session(topic)`: a workflow starts on a topic. A call after a finished
  workflow starts a new one.
- `superpowers_start_brainstorming()`: the brainstorming skill starts.
- `superpowers_request_design_approval(path)`: the design document is written. This opens the
  design gate.
- `superpowers_start_writing_plan()`: the writing-plans skill starts.
- `superpowers_request_plan_approval(path, tasks: [{n, title, files}], checkpoints)`: the plan
  is written, with its numbered tasks and the task numbers after which the agent stops for
  you. This opens the plan gate.
- `superpowers_start_execution(mode)`: execution of the approved plan starts; `mode` is
  `subagent` or `inline`.
- `superpowers_start_task(n, note?)`: task `n` starts.
- `superpowers_write_failing_test(n, note?)`: the failing test of task `n` is written (tdd
  red).
- `superpowers_make_test_pass(n, note?)`: the test of task `n` passes (tdd green).
- `superpowers_start_refactoring(n, note?)`: the refactoring of task `n` starts.
- `superpowers_finish_task(n, note?)`: task `n` is done.
- `superpowers_reach_checkpoint(after_task)`: the agent stops after task `after_task` and
  summarises its progress. This opens a checkpoint gate.
- `superpowers_start_finishing_branch()`: the finishing-a-development-branch skill starts.
- `superpowers_finish_session(outcome)`: the workflow ends; `outcome` is `merged`,
  `pr_opened`, `kept` or `discarded`.

Each report is stored in the session's history. The status the window shows is folded from
those reports, and Partitur rebuilds it from the history when the two disagree, for example
after a crash. Times are shown as `HH:MM` in your local time zone.

## The flow rail

Under the breadcrumb of the Session and Env tabs, a session that reports shows a rail: a
`superpowers` chip and the five steps `brainstorm`, `design`, `plan`, `execute` and `finish`.
Steps before the current one are dimmed and steps after it are hollow. The current step may
carry a detail: the plan's size, the task being worked on (`execute · 4/11`) or the outcome.
Clicking a step shows its document on the Spec tab.

- While a gate waits, the current step turns amber and reads `· waiting for your approval`,
  and the right of the rail holds the approve button with its `Enter` key cap. The button is
  disabled while the gate cannot be approved (see [Gates](#gates)).
- While the plan runs, the right of the rail names the current task and the phase of its
  test-driven cycle (`tdd red`, `tdd green`, `tdd refactor`), followed by an `open plan` link.

## The Spec tab

The Spec tab sits between Session and Env and is disabled for a session that never reported.
It has an outline on the left and a document on the right.

- The outline has one block per step. The current step's block is highlighted; a block reads
  `· gate` while the session waits at that step's gate and `· approved` once you approved its
  document. It shows when brainstorming started (`in transcript · 14:02`), the path of the
  design or the plan, the plan's size, and which task runs or that execution waits for the
  plan's approval. A click on a block shows the document of that step.
- The document shows its title, its revision and the time it was submitted (`rev 2 · 14:02`)
  and a `refresh` link that reads the file again. A plan lists its first four tasks with their
  files, marks finished tasks `done`, and sums up the rest and the checkpoints. Then comes the
  file's text as Markdown; its links open with the session's link opener.
- The file is read in the background, so the document says `reading…` meanwhile. It is read
  again whenever the agent reports a new revision. When the file is missing the document says
  `not found at <path>`, and when it cannot be read it says why. A document the agent has not
  reported yet is named as missing.
- While a gate waits, a bar at the foot has `Approve design`, `Approve plan` or `Continue`, a
  `Request changes` button, an `Open in editor` button for a gate with a document (it hands
  the file to `xdg-open`) and a line that says what approving does.

## The transcript

Reports leave rows in the transcript. Starting a task is a row of its own with the task's
title and files. The phases of the test-driven cycle show as a `tdd` row with a `red`, `green`
or `refactor` pill and the agent's note. Every other report is a small `step` row, such as
`design ready for your approval · docs/design.md`, `executing with subagents`,
`task 3 done`, `checkpoint after task 4 · waiting for your approval` or
`finished · merged`. What you answer at a gate leaves a row too, such as
`Design approved by you at 14:02` or `Changes to the plan requested at 14:02`.

## In the tree

A session that reports has a step column in its row: a dot and the step in words
(`brainstorm`, `design · gate`, `plan · gate`, `execute · 4/11`, `execute · checkpoint`,
`finish · merged`). The dot is amber while a gate waits, bright while the session runs and dim
otherwise. The status line on the Projects tab counts the sessions at a gate on their own, as
`N gates waiting`, apart from the other sessions that ask you.

## Gates

A gate is a point where the agent asks you to decide before it goes on: the design, the plan
or a checkpoint. The agent ends its turn after opening one, and the session then shows
`asks you`.

- `Approve design`, `Approve plan` or `Continue` closes the gate and sends the agent a fixed
  prompt as your message: `Design approved. Write the implementation plan.`, `Plan approved.
  Execute it.` or `Continue.`.
- `Request changes` shows the Session tab with the start of your answer in the prompt box:
  `Changes to the design: `, `Changes to the plan: ` or `Before continuing: `. Write what
  should change and send it. It is disabled for an archived session, which takes no prompt.
- Any prompt you send to a session at a gate closes it, whether you approved or not, so the
  agent always reads your decision as the next message.
- Approval is refused while the session is in a turn, and for an archived session; the
  approve buttons are disabled then, and `Enter` does nothing for the gate. The agent ends
  its turn after opening a gate, so this applies only when a turn is still running.
- Only you close a gate. The agent cannot, and neither can a child's result or an
  orchestrator's prompt.
- An open gate survives a restart: the session starts as `asks you` again, with its gate
  open.

## Keyboard

- `G` opens the Spec tab of the session that has waited longest at a gate, in any open
  project, on every tab. It does nothing when no session waits.
- On the Session and Spec tabs, with the cursor in no field, `Enter` approves the waiting
  gate. On the Session tab, with no gate to approve, it approves an orchestrator's proposed
  split as before. On the Spec tab it never approves a proposal, since the proposal is not
  shown there.

See [Keyboard](keyboard.md) for the rest.

## Orchestrators and children

A gate is only for you, also in an orchestrated project.

- A child that ends its turn at a gate does not report to its orchestrator. Its result is held
  until a turn of the child ends with no gate open, and that turn's final message is reported.
  A child that failed is reported whatever gate is open.
- `send_to_child` to a child at a gate is refused with `child <id> waits for the user's
  approval of its <design|plan|checkpoint>; ask again after the user decided`.
- An orchestrator that waits at a gate of its own takes no child result: the results wait,
  one per turn as always, until you decided the gate.
- A session at a gate marks every session above it in the tree with `child asks you`.
