# Orchestration

A project session in a project with at least two repositories besides the project folder
itself is an orchestrator: it plans the work across the repositories and hands it to child
sessions, one per repository, instead of changing anything itself. In a project with a single
repository, picking `orchestrator · for this repository` in the sheet makes one too; all of
its children then work in that repository, which is the project folder itself when the
folder is the repository.

- Partitur gives an orchestrator five tools through its own MCP server `partitur`, which
  serves every session (plain sessions get the workflow reporting tools of
  [Workflow reporting](workflows.md), `rename_session` and `ask_user`, children get
  `propose_title` in place of `rename_session`, and the core refuses these five from a session
  that is not an orchestrator):
  `propose_children` proposes child sessions (a repository, a prompt and a title each),
  `send_to_child` sends a follow-up prompt to one of its children (queued while the child is
  mid-turn, unless a gate or question is open) or decides the child's gate (see below),
  `list_children` lists its children with their title, repository, session id, status and
  where each stands in its workflow, `cancel_child` stops a child's current turn (its result
  then reaches the orchestrator as a child result marked cancelled) and `get_child_result`
  returns a child's last final message, its status and where it stands. Calls to these tools
  are allowed without asking.
- Each session reaches the server on a URL of its role (`/<session>`,
  `/<session>/orchestrator` or `/<session>/child`), which lists only that role's tools; a
  call of another role's tool is an unknown tool. Every role also gets `list_sessions` and
  `read_session`, which look at the other sessions of the project, and a plain session
  alone gets `propose_session`, which proposes one new session on a one-row card (see
  [Sessions](sessions.md)). `rename_session(title, session_id?)` renames
  the orchestrator, or with `session_id` one of its own children; any other session is
  refused (`<id> is not a child of this session`). The title is put on one line, every run of
  whitespace one space, and cut after 60 characters with `…`; a blank one is refused. It may
  replace a title you typed. Claude Code's own title never replaces it.
  `list_children` marks a title you typed with `set by you`, and the tool's description tells
  the orchestrator to weigh such a title against a child's proposal.
- A child never renames itself: its `rename_session` is refused, and it gets
  `propose_title(title)` instead. Its first prompt starts with a note naming its title and
  telling it to propose a new one with `propose_title` when its work drifts from it. The tool
  answers `sent to the orchestrator` at once and leaves the child's title as it is. The
  proposal, cleaned like a renamed title, reaches the orchestrator as a prompt marked
  `title proposal · <repository> · <title>`, followed by the child's session id, the
  proposed title, `(the current title was set by the user)` when you typed the child's
  current title, and a line saying it may accept the title with `rename_session` or keep the
  current one. Proposals that arrive while the orchestrator is busy wait with the child
  results, delivered one per turn in the order they arrived. A newer proposal of a child
  replaces one of its proposals that still waits, and a proposal names the child's title, and
  whether you typed it, as they are when it goes out.
- Children started before agents could rename sessions keep the title source they were saved
  with: a title the orchestrator gave them and you approved unchanged counts as typed by you,
  so `list_children` and their proposals mark it `set by you`.
- The orchestrator's first prompt starts with a preamble: its role, these tools, the workflow
  folders it may edit in, and the project's repositories. It tells an orchestrator that works
  with the superpowers skills to run brainstorming and writing-plans itself, writing the design
  and the plan into `docs/superpowers/` and opening their gates in its own session, and to
  propose children only once you approved the plan, each given the plan's absolute path and
  the tasks it executes. Checkpoints in the plan stop a child for the orchestrator, not for
  you.
- A proposal appears in the orchestrator's transcript as the `Proposed split` card: a table
  with one row per child (repository, title and prompt) and a muted `skipped` row for every
  repository of the project that gets no child. A click on a child's row turns it into a
  form for its title, prompt and auto-allow toggle, with a button that removes the child; a
  click on a skipped row adds a child for that repository. `Edit split` opens every form at
  once and offers a picker that adds a further child for any repository. `Approve split`
  (or `Enter`, while the cursor is in no field) starts the children as edited; `Reject`
  sends the orchestrator the optional rejection note written next to it (approving carries
  no note). An orchestrator whose turn ends with its split undecided waits for you (`asks
  you` in the tree) until you decide; it still takes your prompts and child results
  meanwhile, and each turn they start ends waiting for you again.
- Every child has branch settings of its own, with the same folder and branch choices (`Main
  checkout` or `Worktree`; `As is`, `Existing` or `New`), rules and messages as the
  new-session sheet, in its form on the card; the row shows its choice and the branch name or
  existing branch. They start from the defaults of the orchestrator's sheet, which offers
  `As is` and `New` only, and a dim `from orchestrator` marks each value that still equals
  them (never the existing branch picked). The orchestrator's branch name is only a default: a
  child making a new branch needs a name of its own, and no two children may make one branch
  name or one worktree folder in a repository (which also keeps two children from adding a
  worktree for one existing branch). Children may share the worktree an existing branch
  already has; the note names the other children first, then other sessions
  (` · also used by "<title>"`). A child that switches its main checkout (`New` or `Existing`
  in `Main checkout`) needs its repository's checked-out folder to itself, as in the
  new-session sheet, and also against the other children (`<repo> is in use by another child
  — use a worktree`). A child that switches the main checkout to a branch another child puts
  in a worktree is refused with `<branch> is checked out in <path> — pick Worktree`. When a
  new branch's name exists already, a `use existing` link beside the problem makes the child
  work on that branch in the same folder; the child's branch choice then differs from the
  orchestrator's, so its `from orchestrator` mark goes.
- The card checks each child in its own repository, in the background. A row says
  `loading…` until git has described its repository, and then names what keeps the child from
  starting, in the failure colour. `Approve split` (and `Enter`) does nothing while a child is
  loading or has such a problem. While one has a problem, a line over the buttons says so even
  when its row is scrolled away: `Cannot approve: <repo>: <problem>`, with `(and <n> more)`
  when other children have one too. A repository on a detached HEAD has no branch to start
  from, so a child there needs a base picked (`pick a base branch`).
- Approving is all or nothing. The git work of every child runs first, in the background:
  `Approve split` says `approving…` and the card is locked, while the rest of the window keeps
  working. Only once all of it succeeded are the children started. When a step fails, what
  the approval made is undone, the proposal stays undecided and the card is editable again
  with the reason below it.
- In the tree an orchestrator starts folded, with one row under it that sums up its
  children: `2 children · api running · web idle`, each child as its repository and status.
  The orchestrator's chevron shows the children's own rows instead; Partitur remembers which
  orchestrators are expanded. Opening a child from elsewhere (the jump panel, a card) expands
  the orchestrators above it.
- Approving starts the children under the orchestrator in the tree and tells the
  orchestrator their session ids, and for each child its mode (`worktree`, `new branch`,
  `checked-out`, `existing branch` or `worktree on existing branch`), the branch it works on
  and the folder it works in (`<title> · <repository> · session <id> · worktree <branch> · in
  <folder>`); rejecting tells it the note. Children run on the
  orchestrator's backend with its model and effort, and their auto-allow toggle starts at the
  orchestrator's.
- When a child's turn ends, its final message reaches the orchestrator as a prompt marked
  `child result · <repository> · <title>`, followed by the child's session id and, for a child
  that reports a workflow, its step as the tree shows it (`execute · 4/11`). Results that
  arrive while the orchestrator is busy wait, and are delivered one per turn in the order they
  arrived. `list_children` adds the same step to each child's line, and with it what the child
  waits for, as in
  `- Fix API · api · session <id> · waiting for you · execute · 4/11 · waits at its checkpoint`;
  `get_child_result` adds that line under its header. A child without a workflow adds no step.
- From the decision on, the orchestrator's transcript is a timeline: your decision, each
  child's result (`<repository> reported back:` over the child's message, or
  `<repository> waits at its <gate>:` and `<repository> asks:` for a child that waits for
  it), what the orchestrator answered, each prompt it sent a child with `send_to_child` (with
  its decision, `approved` or `changes requested`, when it decided a gate, and a link that
  opens the child), each title a child proposed (`<repository> proposes the title
  "<title>"`, with a link that opens the child), your own prompts (with a preview of each
  image they were sent with, which opens the picture, and the names of other files) and the
  prompts Partitur wrote, with the orchestrator's thoughts and tool calls in between. Its
  last line names the children it waits for, a child that waits for its decision among
  them, and the ones that ask you. Above the transcript a strip shows one card per child
  (repository, title, status, context usage) that opens the child.
- `Stop children`, under an orchestrator's prompt while a child is in a turn, stops the turn
  of every child, as `Stop` does for one session: a child that waits for a permission answer
  has the request cancelled and its turn stopped. Each stopped child reports its turn to the
  orchestrator as cancelled.
- An orchestrator may inspect but never change the repositories: its edit, delete, move and
  execute permission requests are denied, with a notice in its transcript. Only its workflow
  documents are its own to write: an edit whose files all lie inside `docs/superpowers/`,
  `.superpowers/` or BMAD's output folder of its working folder is asked like any other
  request, or granted when its auto-allow is on. BMAD's output folder is the `output_folder` of
  the `[core]` table of `_bmad/config.toml` in that folder (a leading `{project-root}/` dropped)
  when it names a relative folder, and `_bmad-output/` otherwise. Partitur reads that file
  anew for each such request, so a change to it counts from the next request on. The files
  must be named by absolute paths, and a `..` or a symbolic link that leads out of a folder,
  or a dangling link, does not count as inside. Read and search requests are asked
  like in any other session, or granted when its auto-allow is on, except that a read (not a
  search) whose files all lie inside the worktrees of its children (symbolic links resolved)
  is granted without asking, so it can look at what its children made. Partitur resolves the
  paths of a read or an edit, and reads BMAD's configuration, in the background, which takes
  a moment: until then the request waits, unanswered and not granted. Its card says
  `checking paths…` and its allow buttons cannot be pressed, and turning on auto-allow
  meanwhile grants nothing, though denying it or `Stop` still answer it.
  When a path cannot be checked at all, the request is decided as one whose files lie
  outside: an edit is denied and a read asked. The denial
  applies only to tool calls that ask Partitur for permission, so calls your own Claude Code
  allow-rules already permit (the settings under the Claude config folder in use) are not
  denied. Likewise Partitur never sees Copilot's own allow-rules (`--allow-tool`, or an
  `allow_always` answer Copilot keeps for a folder), so it cannot deny a call Copilot never
  asks about. Third-party MCP tools, which report the kind `other`, are not denied either.
- An orchestrator on [GitHub Copilot](configuration.md#github-copilot) is also started
  without Copilot's editing and shell tools, so it cannot change anything even where Copilot
  would not ask: once Copilot has said who it is, its process is started once more with
  `--excluded-tools=edit,create,apply_patch,bash,powershell` after the backend's arguments.
  When those arguments already hold `--excluded-tools=<list>`, the tools the list lacks are
  appended to it instead, and when it already names them all, the process is not started
  again. Write your own list in that `=` form: Partitur leaves the space-separated form
  (`--excluded-tools <list>`) alone and adds its own list after it. The first process opens no
  session, and nothing of it shows. Its children start once, with every tool. A Copilot that
  does not say who it is (no `agentInfo` in its `initialize` answer) is not recognised and
  keeps every tool, so only the denial of its permission requests protects the orchestrator.
- A child never waits for you (see
  [Workflow reporting](workflows.md#orchestrators-and-children)). Its first prompt carries,
  after the note on its title, the note `You work for an orchestrator. When your workflow
  opens a gate or you ask with ask_user, end your turn: the orchestrator decides and answers
  in your next prompt.` The orchestrator's preamble tells it
  that its children's gates and questions reach it as `child waits` prompts, to decide them
  with `send_to_child`, to ask you with `ask_user` when you should decide and pass the answer
  on, and never to ask you to open a child.
- A child's turn that ends at a gate, or with a question open (one it asked with `ask_user`),
  reaches the orchestrator at once, through the same queue as the child results, as a prompt
  marked `child waits`. It reads like a child result, with the line of where the child stands
  telling what it waits for, and a closing line that says how to decide:

  ```
  child waits · api · Fix API
  session <id>
  execute · 4/11 · waits at its checkpoint

  <the child's final message>

  Decide with send_to_child(session_id, decision: "approve" or "changes", prompt). Ask the user with ask_user first when the user should make this decision.
  ```

  For a question the third line reads `<step> · asks: <question>`, or `asks: <question>` for
  a child without a workflow, followed by
  `options: <label> · <label>` when it offers options, and the closing line is
  `Answer with send_to_child(session_id, prompt); the prompt is the child's answer. Ask the
  user with ask_user first when the user should answer.` A turn that ends with both a gate
  and a question open is reported as the question, and answering it closes both. A turn
  that failed is reported as a child result, whatever is open.
- `send_to_child(session_id, prompt, decision?)` decides what the child waits for; `decision`
  is `approve` or `changes`. At a gate, once the child's turn ended, `approve` closes the gate
  as your approve button does: the child gets the gate's fixed approval text (such as
  `Continue.` or `Document approved. Continue.`), followed by the prompt after a blank line
  when one is given, and the workflow moves on as on your approval. `changes` needs a prompt
  and sends it after the gate's change prefix (such as `Before continuing: ` or
  `Changes to the plan: `). A question is answered by a prompt without a decision, as if you
  had answered its card. Everything else is refused, with the reason as the tool's answer:
  - a gate without a decision: `child <id> waits at its <gate>; send decision "approve" or
    "changes"`, the gate named in its workflow's word (`design`, `plan` or `checkpoint` for
    superpowers 7, `document`, `story order`, `readiness verdict`, `build plan`, `build`,
    `review verdict` or `retrospective verdict` for BMAD 6.12);
  - a question with a decision: `child <id> asks a question; answer it with a prompt and no
    decision`;
  - a decision while nothing is open: `child <id> has no open gate`;
  - a decision, or a prompt that would answer a question, once the child waits for something
    newer than the `child waits` the orchestrator was told about (you decided in the child
    and it stopped again meanwhile): `child <id> now waits at its <gate>; that wait reaches
    you as a child waits prompt`, or for a question `child <id> now asks another question; it
    reaches you as a child waits prompt`. The newer wait's own `child waits` follows, so a
    decision never closes a gate or answers a question the orchestrator has not seen;
  - a decision, or a prompt to a child at a gate or with a question, while the child is still
    in its turn: `child <id> is still in its turn`;
  - a blank prompt, unless the decision is `approve`: `send_to_child needs a prompt`.

  Without a decision and with nothing open, the prompt is a follow-up: it is sent at once, or
  queued while the child is mid-turn. A queued follow-up goes out when the child's turn ends
  with nothing open; when it ends at a gate or with a question open, the follow-up waits
  until the gate is decided or the question answered or dismissed, so it never drives the
  child past what the orchestrator is about to decide.
- The child's transcript records a decision as the orchestrator's:
  `Checkpoint passed by orchestrator at 14:02`, `Document approved by orchestrator at 14:02`,
  `Changes to the plan requested by orchestrator at 14:02` or
  `Checkpoint answered by orchestrator at 14:02`, and the card of a question it answered reads
  `answered by orchestrator: <text>`. A decision the orchestrator passes on after asking you
  is recorded as its own, since it acted.
- You may still decide in the child, with its gate bar, approve button, `Request changes` or
  question card; that is recorded as yours. A `child waits` of the child still queued is then
  dropped, and if the orchestrator acts on one it already got, a decision is refused with
  `child <id> has no open gate`, or with the `now waits` refusal above once the child waits
  again. `Cancel` on a child's question closes it without an answer in the same way: the
  child becomes `Done`, or runs the follow-up queued for it, and the orchestrator hears from
  it again after its next turn.
- In the tree a child that waits for its orchestrator reads `waits for orchestrator`, not
  `asks you`, and marks none of the sessions above it. `N gates waiting` and `G` count only
  the gates you decide.
- An orchestrator at a gate of its own, or with a question to you open, holds the child
  results, `child waits` and title proposals back, delivered one per turn once you decided or
  answered, so a child's wait never takes over a turn while you have not answered.
- A child whose orchestrator is archived waits for you, as any session does, with `asks you`
  in the tree and its gate counted for `G` and `N gates waiting`. Restoring the orchestrator
  hands the wait to it.
- A child's wait survives a restart. A `child waits` that had not reached the orchestrator
  when Partitur closed is sent after the next start, and one that had is not sent twice. A
  child whose report fails then keeps waiting for you, and the error bar says why.
- A child's permission request stays yours to answer: the orchestrator never grants one. It
  marks every session above the child with `child asks you`, and the orchestrator's session
  shows it as a card above the timeline's last line, `<repository> asks to <title>`, with a
  link that opens the child and a button per option, which answers the request for the
  child. The cards come oldest first, and one leaves once its request is settled, there or
  in the child. A child with a permission request waiting counts as asking you, not as
  waiting for its orchestrator.
- A session waiting for you, one a quit interrupted, or one whose turn failed marks every
  session above it in the tree with `child asks you`.

A headless end-to-end check drives one orchestration against a real agent, in a throwaway
project with the repositories `api` and `web` and a throwaway data folder. It needs a
logged-in Claude Code or Copilot; the adapter inherits the environment, so `CLAUDE_CONFIG_DIR`
(or `COPILOT_HOME`) picks the login. `PARTITUR_E2E_COMMAND` overrides the adapter command
(default `claude-agent-acp`, run through `npx` when it is not installed), and
`PARTITUR_E2E_ARGS` adds arguments after it, split on whitespace. Partitur's instructions are
written to the throwaway data folder and the adapter is given that folder, as the app does.

```sh
CLAUDE_CONFIG_DIR=~/.claude-main cargo run -p partitur --example orchestrate
PARTITUR_E2E_COMMAND=copilot PARTITUR_E2E_ARGS=--acp cargo run -p partitur --example orchestrate
```

It prints every state transition on one line, approves the proposal as proposed, and exits
with status 0 once the orchestrator has both children's results, or 1 after 300 seconds or a
failure. It switches auto-allow on for the orchestrator and for every child, so a headless run
never waits on a permission (the orchestrator's changing calls are still denied). The
temporary folders are removed afterwards, but the adapter keeps the sessions' history for the
temporary project under the Claude config folder it ran with (`$CLAUDE_CONFIG_DIR/projects`),
or Copilot under its `COPILOT_HOME`.

The backend's ignored round-trip test sends one prompt to a real agent the same way, chosen by
the same two variables:

```sh
PARTITUR_E2E=1 cargo test -p partitur-backend-acp --test connection real_adapter -- --ignored
PARTITUR_E2E=1 PARTITUR_E2E_COMMAND=copilot PARTITUR_E2E_ARGS=--acp \
  cargo test -p partitur-backend-acp --test connection real_adapter -- --ignored
```
