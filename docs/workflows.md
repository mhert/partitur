# Workflow reporting

A session that works through the superpowers skills (brainstorming, writing-plans,
executing-plans, subagent-driven-development, test-driven-development,
finishing-a-development-branch) can tell Partitur where it stands. Partitur shows that as a
flow rail above the transcript, a step column in the tree and a Spec tab that holds the
design and the plan, and it holds the agent at every gate until you decide. A session that
never reports looks as before, except that its Spec tab is disabled.

## The tools

Every session gets Partitur's MCP server `partitur`. A plain session lists the
`superpowers_v7_*` tools below and the workflow-level `bmad_v6_12_*` tools of
[BMAD 6.12](#bmad-612); an orchestrator lists its [orchestration tools](orchestration.md) as
well. Calls to them are allowed without asking. Partitur adds a short instruction block per
workflow to the system prompt of every session when it creates or resumes it; each tells the
agent to call that workflow's tools only while it works with it, and to end its turn after a
gate. A path an agent reports is relative to the session's working
folder, which is its worktree when it has one.

Every tool name carries the workflow and its version: `superpowers_v7_` names version 7 of
the superpowers workflow, `bmad_v6_12_` version 6.12 of the BMAD Method. The instruction block tells the agent which of these tools to call,
so it never has to guess a version. The unversioned `superpowers_*` names of earlier
versions of Partitur are unknown tools now; a call of one is answered with an error.

- `superpowers_v7_start_session(topic)`: a workflow starts on a topic. A call after a
  finished workflow starts a new one.
- `superpowers_v7_start_brainstorming()`: the brainstorming skill starts.
- `superpowers_v7_request_design_approval(path)`: the design document is written. This opens
  the design gate.
- `superpowers_v7_start_writing_plan()`: the writing-plans skill starts.
- `superpowers_v7_request_plan_approval(path, tasks: [{n, title, files}], checkpoints)`: the
  plan is written, with its numbered tasks and the task numbers after which the agent stops
  for you. This opens the plan gate.
- `superpowers_v7_start_execution(mode)`: execution of the approved plan starts; `mode` is
  `subagent` or `inline`.
- `superpowers_v7_start_task(n, note?)`: task `n` starts.
- `superpowers_v7_write_failing_test(n, note?)`: the failing test of task `n` is written
  (tdd red).
- `superpowers_v7_make_test_pass(n, note?)`: the test of task `n` passes (tdd green).
- `superpowers_v7_start_refactoring(n, note?)`: the refactoring of task `n` starts.
- `superpowers_v7_finish_task(n, note?)`: task `n` is done.
- `superpowers_v7_reach_checkpoint(after_task)`: the agent stops after task `after_task` and
  summarises its progress. This opens a checkpoint gate.
- `superpowers_v7_start_finishing_branch()`: the finishing-a-development-branch skill
  starts.
- `superpowers_v7_finish_session(outcome)`: the workflow ends; `outcome` is `merged`,
  `pr_opened`, `kept` or `discarded`.

Each report is stored in the session's history. The status the window shows is folded from
those reports, and Partitur rebuilds it from the history when the two disagree, for example
after a crash. Times are shown as `HH:MM` in your local time zone. A history and a session
record written by an earlier version of Partitur still load: their reports read as reports of
the superpowers workflow, and the status stored with such a session keeps its open gate, so
the session still waits for you, and is rebuilt from the history the first time you open it.

## BMAD 6.12

A session can also run one workflow of the BMAD Method, version 6.12, normally as an
orchestrator's child. BMAD runs each workflow in a fresh chat, so a session runs one, and its
instruction block says so. A session that starts a BMAD workflow after superpowers reports, or
the other way round, shows only the workflow it reported last; the open gate of the other one
is dropped.

The workflow-level tools, offered to every session:

- `bmad_v6_12_start_workflow(kind, persona?, story?, epic?)`: the session's workflow starts,
  from scratch. `kind` is `brief`, `prfaq`, `prd`, `ux`, `architecture`, `epics_and_stories`,
  `sprint_planning`, `spec`, `build`, `code_review` or `retrospective`; `persona` names the
  BMAD agent that runs it, such as `Amelia · dev`.
- `bmad_v6_12_request_document_approval(doc, path)`: a planning document or the spec is
  written; `doc` is `brief`, `prfaq`, `prd`, `ux`, `architecture` or `spec`. This opens the
  document gate.
- `bmad_v6_12_request_story_order_approval(path, stories: [{id, title}])`: the epics and
  stories are written, with the stories in their order. This opens the story order gate.
- `bmad_v6_12_request_readiness_decision(verdict, path)`: sprint planning's readiness check
  has its verdict, `pass`, `concerns` or `fail`. This opens the readiness gate.
- `bmad_v6_12_request_build_plan_approval(story, path)`: the plan for building a story is
  written. This opens the build plan gate.
- `bmad_v6_12_request_build_review(story, path?)`: the story is built and waits for your
  review before its pull request. This opens the build review gate.
- `bmad_v6_12_request_review_verdict(story, verdict, path)`: the code review has its verdict,
  `approved` or `changes_requested`. This opens the review verdict gate.
- `bmad_v6_12_request_retrospective_verdict(epic, verdict, path)`: the retrospective has its
  verdict, `accepted`, `accepted_with_open_items` or `rejected`. This opens the retrospective
  verdict gate.
- `bmad_v6_12_finish_workflow(outcome?)`: the workflow ends, with how it ended in the agent's
  words. It closes any open gate.

Each kind has its steps: `draft · approve` for the planning documents and the spec,
`draft · story order` for epics and stories, `plan · readiness` for sprint planning,
`plan · build · review` for a build, `review · verdict` for a code review and
`retro · verdict` for a retrospective. A workflow starts at its first step. A request moves it
to the step its gate sits on and records the file it names as a new revision; approving the
gate marks that file approved and moves the workflow to the next step, if there is one. A
request before any `start_workflow` starts the workflow its gate belongs to, such as a build
for a build plan.

The gates, with what approving sends and what `Request changes` puts in the prompt box:

| Gate | Step | Button | Sent on approval | Request changes |
|---|---|---|---|---|
| document | approve | `Approve document` | `Document approved. Continue.` | `Changes to the document: ` |
| story order | story order | `Approve story order` | `Story order approved. Continue.` | `Changes to the story order: ` |
| readiness | readiness | `Accept verdict` | `Readiness verdict accepted. Continue.` | `Before continuing: ` |
| build plan | plan | `Approve plan` | `Plan approved. Build it.` | `Changes to the plan: ` |
| build review | review | `Approve build` | `Build approved. Open the pull request.` | `Changes to the build: ` |
| review verdict | verdict | `Accept verdict` | `Verdict accepted. Continue.` | `Changes to the review: ` |
| retrospective verdict | verdict | `Accept verdict` | `Verdict accepted. Continue.` | `Changes to the retrospective: ` |

What you answer leaves a row in the transcript, such as `Build plan approved by you at 14:02`,
`Review verdict accepted by you at 14:02` or `Changes to the story order requested at 14:02`.
For now a BMAD session's rail shows the `bmad 6.12` chip with the approve button while a gate
waits, its tree cell names the kind (`build`, `code review`), and its Spec tab lists the files
its gates asked about.

## The flow rail

Under the breadcrumb of the Session, Spec and Env tabs, a session that reports shows a
rail: a `superpowers 7` chip and the five steps `brainstorm`, `design`, `plan`, `execute` and
`finish`. Steps before the current one are dimmed and steps after it are hollow. The current
step may carry a detail: the plan's size, the task being worked on (`execute · 4/11`) or the
outcome. Clicking a step shows its document on the Spec tab.

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
  and a `refresh` link that reads the file again. A plan lists all its tasks with their files,
  marks finished tasks `done`, and names the checkpoints after them. Then comes the
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
(`superpowers 7` before the first step, then `brainstorm`, `design · gate`, `plan · gate`, `execute · 4/11`, `execute · checkpoint`,
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
  `Changes to the design: `, `Changes to the plan: ` or `Before continuing: `, put before
  anything you had typed there already. Write what should change and send it. It is
  disabled for an archived session, which takes no prompt.
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
  gate. While a gate is open but cannot be approved, it does nothing. On the Session tab,
  with no gate open, it approves an orchestrator's proposed split as before. On the Spec tab
  it never approves a proposal, since the proposal is not shown there.

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
