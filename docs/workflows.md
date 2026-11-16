# Workflow reporting

A session that works through a workflow Partitur knows can tell Partitur where it stands.
Partitur knows two: version 7 of the superpowers skills (brainstorming, writing-plans,
executing-plans, subagent-driven-development, test-driven-development,
finishing-a-development-branch) and version 6.12 of the [BMAD Method](#bmad-612). It shows
where the session stands as a flow rail above the transcript, a step column in the tree and a
Spec tab that holds the workflow's documents, such as the design and the plan, and it holds
the agent at every gate until you decide. A session that never reports looks as before,
except that its Spec tab is disabled. The sections after BMAD's describe the superpowers
workflow where they name its steps, buttons and texts; BMAD's own are described in its
sections.

## The tools

Every session gets Partitur's MCP server `partitur`. A plain session lists the
`superpowers_v7_*` tools below and the workflow-level `bmad_v6_12_*` tools of
[BMAD 6.12](#bmad-612); an orchestrator lists its [orchestration tools](orchestration.md) and
BMAD's [run tools](#a-bmad-run) as well. Calls to them are allowed without asking. Partitur adds a short instruction block per
workflow to the system prompt of every session when it creates or resumes it; each tells the
agent to call that workflow's tools only while it works with it, and to end its turn after a
gate. One more block, for `ask_user`, tells every agent to ask a question that way and to end
its turn after asking. A path an agent reports is relative to the session's working
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
  `pr_opened`, `kept` or `discarded`. It closes any open gate.

Each report is stored in the session's history. The status the window shows is folded from
those reports, and Partitur rebuilds it from the history when the two disagree, for example
after a crash. Times are shown as `HH:MM` in your local time zone. A history and a session
record written by an earlier version of Partitur still load: their reports read as reports of
the superpowers workflow, and the status stored with such a session keeps its open gate, so
the session still waits for you, and is rebuilt from the history the first time you open it. A
stored status that cannot be read at all, for example one written by a newer version, does not
keep the project from loading: the session loads without a status, and its status is rebuilt
from the history the first time you open it.

## BMAD 6.12

A session can also run one workflow of the BMAD Method, version 6.12, normally as an
orchestrator's child. BMAD runs each workflow in a fresh chat, so a session runs one, and its
instruction block says so. A session switches workflows when it starts the other one: a BMAD
workflow or run that starts after superpowers reports, or a superpowers session that starts
after BMAD reports, replaces the view, and the open gate of the old one is dropped. Any other
report of the other workflow is stored in the history but leaves the view and its gate as they
are. A session with no workflow yet opens the workflow of its first report, whichever it is.

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
gate marks that file approved and moves the workflow to the step after the gate's own, if the
kind has that step and there is a next one. Answering a gate that no longer waits changes
nothing. A request before any `start_workflow` starts the workflow its gate belongs to, such
as a build for a build plan.

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
A BMAD session's rail has the chip `bmad 6.12 · <kind>`, such as `bmad 6.12 · build`, and the
kind's steps; the steps before the current one are done, and all of them once the workflow
finished. A click on a step opens the file its gate asked about last. While a gate waits, the
right of the rail holds the approve button; otherwise it names the persona and the story
(`story 1.2`) or else the epic (`epic 2`) it works on.

Its tree cell names the kind and what it waits for, `approval` at a document,
`story order`, `readiness`, `plan gate` and `review gate` in a build, and `verdict` at a
code review's or retrospective's verdict (`build · plan gate`, `code review · verdict`).
Without a gate it names the current step (`build · build`), or `done` once the workflow
finished. Before any workflow it reads `bmad 6.12`.

Its Spec tab's outline has one block per step, marked `· gate` while that step's gate waits and
`· approved` once you approved its file, with the file's path and, at a verdict gate, the
verdict: `pass`, `concerns`, `fail`, `approved`, `changes requested`, `accepted`,
`accepted with open items` or `rejected`. Its documents are the files its gates asked about, in
the order they were first asked about; the story order lists its stories as tasks.

In the transcript, a workflow started with a persona leaves a row naming the persona and the
kind with its story, such as `Amelia · dev  build · story 1.2`. Every other report leaves a
step note: `bmad · <kind>` for a start without a persona,
`prd ready for your approval · docs/prd.md` for each file waiting for you
(`build ready for your review` for a build review without a file), and `finished` or
`finished · <outcome>` at the end.

### A BMAD run

An orchestrator leads a whole BMAD run: it runs no workflow itself, proposes one child per
BMAD workflow and reports where the run stands with the run tools. Only an orchestrator is
offered them: on a plain session's URL they are unknown tools, and a run report that still
reaches Partitur from a session that is not an orchestrator is refused with
`this session is not an orchestrator`. The instruction block says both.

- `bmad_v6_12_start_run(topic, path)`: the run starts, from scratch. `path` is `single` for
  one change, `epic` for one epic or `project` for a whole project.
- `bmad_v6_12_start_planning(doc, persona?)`: the run starts on a planning document, `brief`,
  `prfaq`, `prd`, `ux` or `architecture`.
- `bmad_v6_12_plan_stories(epics: [{id, title, stories: [{id, title}]}])`: the epics and their
  stories are planned, in their order.
- `bmad_v6_12_start_sprint_planning(persona?)`: sprint planning starts.
- `bmad_v6_12_start_epic(id)`: the run starts on an epic.
- `bmad_v6_12_start_story(id, path?, persona?)`: the build of a story starts; `path` is the
  story's file.
- `bmad_v6_12_start_story_review(id, persona?)`: the story's code review starts.
- `bmad_v6_12_finish_story(id, outcome)`: the story's review ends, `done` or
  `changes_requested`.
- `bmad_v6_12_start_retrospective(epic, persona?)`: the retrospective of an epic starts.
- `bmad_v6_12_finish_run(outcome)`: the run ends, `completed` or `abandoned`.

Each path has its fixed steps: `spec · build` for a single change, `spec · stories · retro`
for an epic, and `prd · architecture · epics · sprint · stories · retro` for a project, where
a brief or a PRFAQ joins before the PRD and a UX design between the PRD and the architecture
once the run reports one. A run starts at its path's first step. Starting a planning document
makes it the current step on the project path; the single and epic paths have no planning
steps, so there `start_planning` only records the persona and leaves the rail as it is.
Starting sprint planning or the retrospective makes it the current step; planning the stories
moves a project to `epics`. Starting a story moves the run to `stories` (to `build` on the
single path) with the story in its build phase, and makes the epic that lists the story the
current one. Its review moves it to the review phase, but only when `start_story_review` names
the current story; a review of another story changes nothing. Finishing it `done` counts it as
done, while `changes_requested` sends it back to the build. Finishing another story than the
current one leaves the current story's phase alone: `done` still counts that story as done,
and `changes_requested` changes nothing. A persona a report names stays the
run's persona until a later report names another. Run reports never open or close a gate, and
a run report before `start_run` changes nothing.

Starting a run and starting a workflow each start the session's BMAD view over: a
`start_workflow` in a session with a run replaces the run, and a `start_run` replaces the
workflow.

The orchestrator's rail has the chip `bmad 6.12` and the run's steps, those before the current
one done and all of them once the run ended. The `stories` step is a loop box: a rounded box
in the chain of steps whose border is dim before and after the loop and in the text colour
while the loop is current. Inside it stand where the current story is and the phases
`build → review`, the story's phase at medium weight. Where the story is reads:

- `story 3 of 8` on the epic path, counting all stories;
- `epic 2 of 4 · story 3 of 8` on the project path, counting the stories of the current epic;
- `story ? of 8` (or `epic 2 of 4 · story ? of 8` once an epic started) before any story, and
  `stories` before the stories are planned;
- `story 9.9`, without a count, for a story no epic lists.

While a gate waits, the right of the rail holds the approve button. Otherwise it names the
run's persona (`Quinn · qa`), the current story's title (or `story 9.9` for an unlisted one)
and, once `start_story` gave the story's file, an `open story` link that shows the file on
the Spec tab.

The tree cell names the current step (`prd`, `epics`, `sprint`, `retro`), the loop as
`story 3/8 · review` (`story 3/8` once the story is done), `run · completed` or
`run · abandoned` once the run ended, and `bmad 6.12` before any step. The outline has one
block per step; the `STORIES` block notes how many stories of each epic are done
(`epic 2 · 3 of 8 done`) and shows the story list once stories are planned. The Spec tab lists
the story list, each story marked done or current, and the current story's file. The file is
at revision 1 when its story starts and one revision further each time a story starts again
on the same file, so the Spec tab reads it again.

A run report that names a persona leaves a transcript row naming it and what it works on:
`prd`, `sprint planning`, `story 1.2 · <title>`, `review of story 1.2` or `retro · epic 2`.
A review of a story that is not the current one leaves no row. Without a row, these reports
leave a step note:

- `start_run`: `bmad · <topic> · <path>`;
- `plan_stories`: `<e> epics · <s> stories planned`;
- `start_epic`: `epic <id>`;
- `start_story`: `story <id> started`;
- `finish_story`: `story <id> done` or `story <id> back to build`;
- `finish_run`: `run completed` or `run abandoned`.

## The flow rail

Under the breadcrumb of the Session, Spec and Env tabs, a session that reports shows a
rail of its workflow's steps; a BMAD session's rail is described [above](#bmad-612). A
superpowers 7 session's rail has a `superpowers 7` chip and the five steps `brainstorm`,
`design`, `plan`, `execute` and `finish`. Steps before the current one are dimmed and steps
after it are hollow. The current step may carry a detail: the plan's size, the task being
worked on (`execute · 4/11`) or the outcome. Clicking a step shows its document on the Spec
tab. In every workflow, steps that do not fit in a narrow window are cut off before the right
of the rail, never drawn over it.

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
  document. In superpowers 7 it shows when brainstorming started (`in transcript · 14:02`),
  the path of the design or the plan, the plan's size, and which task runs or that execution
  waits for the plan's approval. A click on a block shows the document of that step.
- The document shows its title, its revision and the time it was submitted (`rev 2 · 14:02`)
  and a `refresh` link that reads the file again. A plan lists all its tasks with their files,
  marks finished tasks `done`, and names the checkpoints after them. Then comes the
  file's text as Markdown; its links open with the session's link opener.
- The file is read in the background, so the document says `reading…` meanwhile. It is read
  again whenever the agent reports a new revision. When the file is missing the document says
  `not found at <path>`, and when it cannot be read it says why. A document without a file,
  such as a BMAD run's story list, shows its name as the title and its rows, with no
  revision, no `refresh` link and no text. Any other document the agent has not reported yet
  is named as missing.
- While a gate waits, a bar at the foot has the gate's approve button (`Approve design`,
  `Approve plan` or `Continue` in superpowers 7, the buttons in the [gate table](#bmad-612)
  in BMAD 6.12), a `Request changes` button, an `Open in editor` button for a gate with a
  document (it hands the file to the system's default handler) and a line that says what approving does.

## The transcript

Reports leave rows in the transcript; BMAD's are described [above](#bmad-612). In superpowers
7, starting a task is a row of its own with the task's title and files. The phases of the
test-driven cycle show as a `tdd` row with a `red`, `green` or `refactor` pill and the
agent's note. Every other report is a small `step` row, such as
`design ready for your approval · docs/design.md`, `executing with subagents`,
`task 3 done`, `checkpoint after task 4 · waiting for your approval` or
`finished · merged`. What you answer at a gate leaves a row too, such as
`Design approved by you at 14:02` or `Changes to the plan requested at 14:02`. A child's gate
its orchestrator decided says so: `Checkpoint passed by orchestrator at 14:02`,
`Changes to the plan requested by orchestrator at 14:02`,
`Checkpoint answered by orchestrator at 14:02` or, in BMAD,
`Document approved by orchestrator at 14:02`.

## In the tree

A session that reports has a step column in its row: a dot and the step in words. In
superpowers 7 it reads `superpowers 7` before the first step, then `brainstorm`,
`design · gate`, `plan · gate`, `execute · 4/11`, `execute · checkpoint` and
`finish · merged`; BMAD's cells are described [above](#bmad-612). The dot is amber while a
gate waits for you, bright while the session runs and dim otherwise, as for a child whose
gate waits for its orchestrator. The status line on the Projects tab counts the sessions at a
gate you decide on their own, as `N gates waiting`, apart from the other sessions that ask
you; a child that waits for its orchestrator counts in neither.

## Gates

A gate is a point where the agent asks you to decide before it goes on: in superpowers 7 the
design, the plan or a checkpoint, in BMAD 6.12 the gates in its [gate table](#bmad-612). The
agent ends its turn after opening one, and the session then shows `asks you`. The gates of an
orchestrator's child go to the orchestrator instead (see
[Orchestrators and children](#orchestrators-and-children)). These rules hold for both
workflows; the buttons and texts named here are superpowers 7's, and BMAD's are in its table.

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
- Only you approve a gate, and for a child also its orchestrator. The agent cannot approve
  its own gate, and neither can a child's result. The agent can only drop an open gate, by
  finishing its workflow (`superpowers_v7_finish_session` or `bmad_v6_12_finish_workflow`) or
  starting one over; that closes the gate without approving what it asked about.
- An open gate survives a restart: the session starts as `asks you` again, with its gate
  open, and a child's as `waits for orchestrator`.

## Keyboard

- `G` opens the Spec tab of the session that has waited longest at a gate you decide, in any
  open project, on every tab; a child whose gate waits for its orchestrator is skipped. It
  does nothing when no session waits for you at a gate.
- On the Session and Spec tabs, with the cursor in no field, `Enter` approves the waiting
  gate. While a gate is open but cannot be approved, it does nothing. On the Session tab,
  with no gate open, it approves an orchestrator's proposed split as before. On the Spec tab
  it never approves a proposal, since the proposal is not shown there.

See [Keyboard](keyboard.md) for the rest.

## Orchestrators and children

A child never waits for you. Its gates, and the questions it asks with `ask_user`, go to its
orchestrator, which decides them or asks you in its own session. You may still open a child
and decide there, but you never have to.

- A child's first prompt tells it that it works for an orchestrator: when its workflow opens a
  gate or it asks a question, it ends its turn, and the orchestrator decides and answers in its
  next prompt. The orchestrator's preamble tells it that its children's gates and questions
  reach it as `child waits` prompts, how to decide them with `send_to_child`, to ask you with
  `ask_user` when you should decide and to pass your answer on, and never to ask you to open
  a child.
- A child whose turn ends at a gate or with a question open reports to its orchestrator at
  once, as a prompt marked `child waits`, with its step and what it waits for. The
  orchestrator approves the gate or asks for changes with `send_to_child` and a decision,
  which works like your approve button or `Request changes`: the child gets the same fixed
  prompt or change prefix, and a document's approval counts the same. It answers a question
  with a prompt, as if you had answered the card. A child that failed is reported as a child
  result whatever is open. [Orchestration](orchestration.md) describes the prompt, the
  decisions and the refusals in full.
- The decision is recorded as the orchestrator's: the child's transcript says
  `Checkpoint passed by orchestrator at 14:02` instead of `by you`, and the orchestrator's
  timeline shows the prompt it sent with the decision named and a link that opens the child.
  A decision it passes on after asking you is recorded as its own too, since it acted.
- You can still decide in the child: its gate bar, approve button, `Request changes` and
  question card work as in any session, and what you decide there is recorded as yours. A
  `child waits` of that child still queued for the orchestrator is dropped, and the child's
  next result reaches the orchestrator marked `(turn started by the user)`.
- A child that waits for its orchestrator reads `waits for orchestrator` in muted letters in
  the tree, the jump panel and its own session, not `asks you`. Its step dot is dim, it marks
  none of the sessions above it with `child asks you`, and neither `N gates waiting` nor `G`
  counts it: they count only the gates you decide, an orchestrator's own and those of
  sessions without an orchestrator.
- An orchestrator that waits at a gate of its own, or for your answer to a question it asked,
  takes no generated prompt: child results, `child waits` and title proposals wait, one per
  turn as always, until you decided or answered.
- A child whose orchestrator is archived waits for you as any session does: its gate or
  question shows `asks you`, and its gate counts for `G` and `N gates waiting`. Restoring
  the orchestrator hands the wait back to it, and it hears of the child then.
- A wait survives a restart: a `child waits` that had not reached the orchestrator when
  Partitur closed is sent after the next start, and one that had is not sent twice. A child
  whose report fails then keeps waiting for you, and the error bar says why.
- A session that waits for you marks every session above it in the tree with
  `child asks you`. A child's permission request stays yours to answer, so it marks its
  orchestrator too, and the orchestrator's session shows it as a card from which you answer
  it for the child.
- These rules hold for every workflow alike, so a BMAD child at its build plan's gate reports
  to its orchestrator as a `child waits` and is decided with `send_to_child`, and `G` finds
  the oldest gate you decide whichever workflow opened it. A BMAD run lives on the
  orchestrator; each workflow of the run, such as a story's build or code review, runs in a
  child of it.
- An orchestrator's Spec tab lists, after its own documents and outline, every document its
  children reported, oldest child first and archived children included. Each is named
  `<child title> · <document>`, its outline block is headed by the child's title and names
  the document, and its path is read from the child's folder, so a document a child wrote in
  its worktree opens from there.
