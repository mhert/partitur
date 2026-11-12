# Orchestration

A project session in a project with at least two repositories besides the project folder
itself is an orchestrator: it plans the work across the repositories and hands it to child
sessions, one per repository, instead of changing anything itself. In a project with a single
repository, picking `orchestrator · for this repository` in the sheet makes one too; all of
its children then work in that repository, which is the project folder itself when the
folder is the repository.

- Partitur gives an orchestrator five tools through its own MCP server `partitur`:
  `propose_children` proposes child sessions (a repository, a prompt and a title each),
  `send_to_child` sends a follow-up prompt to one of its children (queued while the child is
  mid-turn), `list_children` lists its children with their title, repository, session id and
  status, `cancel_child` stops a child's current turn (its result then reaches the
  orchestrator as a child result marked cancelled) and `get_child_result` returns a child's
  last final message and its status. Calls to these tools are allowed without asking.
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
- Every child has branch settings of its own, with the same modes, rules and messages as the
  new-session sheet, in its form on the card; the row shows its mode and branch name. They
  start from the defaults of the orchestrator's sheet, and a dim `from orchestrator` marks
  each value that still equals them. The orchestrator's branch name is only a default: a
  child in `Worktree` or `New branch` mode needs a name of its own, and no two children may
  use one name or one worktree folder in a repository. A `New branch` child needs its
  repository's checked-out folder to itself, as in the new-session sheet, and also against
  the other children (`<repo> is in use by another child — use a worktree`).
- The card checks each child in its own repository, in the background. A row says
  `loading…` until git has described its repository, and then names what keeps the child from
  starting. `Approve split` (and `Enter`) does nothing while a child is loading or has such a
  problem.
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
  orchestrator their session ids, and for each child its mode, the branch it made and the
  folder it works in (`<title> · <repository> · session <id> · worktree <branch> · in
  <folder>`); rejecting tells it the note. Children run on the
  orchestrator's backend with its model and effort, and their auto-allow toggle starts at the
  orchestrator's.
- When a child's turn ends, its final message reaches the orchestrator as a prompt marked
  `child result · <repository> · <title>`, followed by the child's session id. Results that
  arrive while the orchestrator is busy wait, and are delivered one per turn in the order they
  arrived.
- From the decision on, the orchestrator's transcript is a timeline: your decision, each
  child's result (`<repository> reported back:` over the child's message), what the
  orchestrator answered, each prompt it sent a child with `send_to_child` (with a link that
  opens the child), your own prompts and the prompts Partitur wrote, with the orchestrator's
  thoughts and tool calls in between. Its last line names the children it waits for and the
  ones that ask you. Above the transcript a strip shows one card per child (repository,
  title, status, context usage) that opens the child.
- `Stop children`, under an orchestrator's prompt while a child is in a turn, stops the turn
  of every child, as `Stop` does for one session: a child that waits for a permission answer
  has the request cancelled and its turn stopped. Each stopped child reports its turn to the
  orchestrator as cancelled.
- An orchestrator may inspect but never change: its edit, delete, move and execute permission
  requests are denied, with a notice in its transcript. Read and search requests are asked
  like in any other session, or granted when its auto-allow is on, except that a read (not a
  search) whose files all lie inside the worktrees of its children (symbolic links resolved)
  is granted at once, so it can look at what its children made without asking. The denial
  applies only to tool calls that ask Partitur for permission, so calls your own Claude Code
  allow-rules already permit (the settings under the Claude config folder in use) are not
  denied. Neither are third-party MCP tools, which report the kind `other`.
- A session waiting for you marks every session above it in the tree with `child asks you`.

A headless end-to-end check drives one orchestration against the real adapter, in a
throwaway project with the repositories `api` and `web` and a throwaway data folder. It needs
a logged-in Claude Code; the adapter inherits the environment, so `CLAUDE_CONFIG_DIR` picks the
login. `PARTITUR_E2E_COMMAND` overrides the adapter command (default `claude-agent-acp`, run
through `npx` when it is not installed).

```sh
CLAUDE_CONFIG_DIR=~/.claude-main cargo run -p partitur --example orchestrate
```

It prints every state transition on one line, approves the proposal as proposed, and exits
with status 0 once the orchestrator has both children's results, or 1 after 300 seconds or a
failure. It switches auto-allow on for the orchestrator and for every child, so a headless run
never waits on a permission (the orchestrator's changing calls are still denied). The
temporary folders are removed afterwards, but the adapter keeps the sessions' history for the
temporary project under the Claude config folder it ran with (`$CLAUDE_CONFIG_DIR/projects`).
