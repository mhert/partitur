# Sessions

## Starting a session

`+ new session` in the tree, or `N` on the Projects tab, opens the new-session sheet for a
project; the repository is picked in the sheet. A project has one `+ new session` row; for a
project with several repositories it reads `orchestrator or a single repo — chosen in the
sheet`. The sheet picks the session's settings before it exists.

- `Repo` is the first field. It picks what the session works on: `orchestrator · all N repos`
  for a project with several repositories, or one repository by name, followed by
  `· AGENTS.md` and `· CLAUDE.md` for the instruction files it has. A folder with one
  repository offers the folder and that repository; a project folder that is itself a
  repository offers just the folder. A project with a single repository, either way, also
  offers `orchestrator · for this repository` last; the plain session stays the default.
  Picking another repository, or the orchestrator, restarts the agent there; the settings
  you picked stay. The subline reads
  `N repos · children of an orchestrator inherit these settings` when the project has
  several repositories, and just `N repos` (`1 repo` for one) otherwise.
- Model and Effort come from the agent itself: Partitur starts it in the background as soon
  as the sheet opens, and both pickers say `loading…` until it answers.
- The settings start from the project's newest session, and from the default backend when
  the project has none. The remembered model is switched on in the agent as soon as it
  answers, so the Effort list is that model's. Picking a model refreshes the Effort list the
  same way, and picking another backend restarts the agent there.
- Effort keeps the remembered or picked value wherever the model's Effort list offers it,
  and takes the agent's own effort otherwise.
- `Auto-allow tool calls` grants every permission request of the session without asking.
- `Create session` (or `Enter`) adds the session to the tree, selects it and focuses its
  prompt. `Cancel`, `Escape` and a click on the backdrop close the sheet and leave nothing
  behind.
- The `Branch & worktree` box decides how the session uses git. Its `mode` is one of:
  - `Checked-out` works in the repository as it is now. The box says which branch that is
    (`branch  <name>  works in the repository as it is now`, `(detached)` without one) and
    runs no git command.
  - `New branch` creates a branch and switches the repository itself to it; uncommitted
    changes come along, as with `git switch -c`. The box notes this.
  - `Worktree` creates the branch in a new worktree and runs the agent there; the box shows
    the folder under `worktree at`, with the home folder written as `~`.
- `branch name` and `base branch` apply to the two modes that create a branch. The name is
  trimmed and must be a valid git branch name that does not exist in the repository yet. The
  base is picked from the repository's local branches. A Worktree must not exist as a folder
  already. What is wrong shows under the box and keeps `Create session` from working:
  `enter a branch name`, `"<name>" is not a valid git branch name`,
  `branch <name> already exists in <repo>`, `<path> already exists`.
- `Fetch <remote> and start from <remote>/<base>` fetches the base's upstream remote (else
  `origin`) first, if that remote has the base, and starts the branch from `<remote>/<base>`.
  The local base branch is never moved. Without such a remote branch the box says
  `<base> has no remote counterpart` and the fetch cannot be checked. A fetch that takes more
  than a minute is stopped and fails the create.
- The box starts from the project's newest session: its mode and fetch choice. Without one it
  starts `Checked-out` with fetching on. The name always starts empty, and the base starts at
  the branch checked out now.
- A repository's checked-out folder is shared. While a live (not archived) session works in
  it, `New branch` there is refused with `<repo> is in use by "<session title>" — use a
  worktree`, since switching the branch would pull it from under that session.
- Git runs in the background. The branches are read when the sheet opens (the rows say
  `loading…`, and `reading the branches of <repo>…` stops `Create session` meanwhile;
  `Checked-out` can be created at once). `Create session` then makes the branch or worktree
  in the background: the sheet freezes, the button says `creating…` and `Cancel` stays
  available. The session is created only once git succeeded; when a step fails, nothing is
  created and the sheet says why. `Cancel` meanwhile closes the sheet and undoes what git
  made (the worktree and branch, or the switch back to the previous branch).
- Only one changing git request runs per repository at a time. While another one (including
  the undo of a cancelled create) is still running there, `Worktree` and `New branch` for that
  repository are refused with `<repo> is busy with another branch change`.
- A project folder that is not a repository offers only `Checked-out`, and says so. So does a
  repository whose git state cannot be read.
- On an orchestrator's sheet the box sets the defaults for its children: `Checked-out` means
  `children work in their repositories as they are now`; the other modes show the common
  base branch (the branches every repository has; `per child` when there are none), whether
  to `Fetch first and start from each repository's remote base`, and a branch name that may
  stay empty because each child can be named on the split card. There is no `worktree at`
  line.
- Git work still running when the window closes finishes, but no session is recorded for it.
- A session is titled `untitled` until the agent titles it: Claude Code sends a short title
  after the first prompt, and Partitur takes it. A title you typed, or one an orchestrator
  gave a child, is never replaced; a child whose title the split card derived from its prompt
  and you left alone takes the agent's title. The first prompt is kept as it was typed, with
  the skill it starts with, and does not change the title.

## Interrupted sessions

When Partitur closes while a session runs or waits for you to answer a permission request,
that turn never ends. On the next start the session shows `interrupted`, and its transcript
says `Partitur closed before this turn ended`. The mark stays until the session's next turn:
a prompt resumes the session as it would an idle one. A session whose turn finished or failed
is not interrupted, and neither is an orchestrator whose turn ended on a proposal you had not
decided yet. A session whose turn ended at a [workflow gate](workflows.md#gates) still asks
you after the restart, with its gate open. A stop counts only once the agent confirmed it:
quitting right after pressing stop can still leave the session interrupted.

## Archiving sessions

A finished session can leave the tree without losing anything. The highlighted row, and the
row under the pointer, end in an `Archive` button; `A` does the same for the highlighted row.
A sheet asks for confirmation first, and only on confirmation does anything change.

- `Done`, `Failed`, `Interrupted` and idle sessions archive directly. A session that runs or
  waits for you offers `Stop and archive`: confirming stops its turn first, the way the stop
  button does.
- An orchestrator archives together with its children. A child cannot be archived on its own;
  its button cannot be pressed and says to archive the orchestrator.
- Nothing is deleted: the transcript and the Env data stay. Archived sessions move under an
  `archived · N` row at the end of their project, collapsed at first, which lists each with
  the date it was archived. The row under the pointer ends in a `Restore` and a `Delete`
  button. An archived orchestrator's children are summed up in one row, as in the live
  tree, until its chevron shows them.
- An archived session opens read-only: the Session, Spec and Env tabs show a banner with a
  `Restore` button, and the Session tab has no composer.
- Restoring puts the session, and its children, back in the tree with the status they had.
  A child's result that came in while its orchestrator was archived is sent to it then, as
  long as Partitur kept running in between; like the results held for a closed project,
  it is not kept across a restart.
- The status line counts live sessions only and says how many are archived; the jump panel
  leaves archived sessions out unless `include archived` is checked.

Partitur does not archive anything on its own.

## Deleting sessions

A session you no longer need can be deleted for good. Next to `Archive`, the highlighted row
and the row under the pointer end in a `Delete` button; `D` does the same for the highlighted
row. An archived row shows its `Delete` button beside `Restore` while the pointer is over
it, and `D` works on the highlighted archived row too. A
sheet asks for confirmation first, and only on confirmation does anything change.

- A session that runs or waits for you offers `Stop and delete`: confirming ends its turn as
  its connection closes.
- An orchestrator is deleted together with its children. A child cannot be deleted on its
  own; its button cannot be pressed and says to delete the orchestrator.
- For every session of the family that made a branch (in `Worktree` or `New branch` mode),
  the sheet shows a block headed `<repository> · <title>` with two boxes, both unchecked:
  `Remove worktree <path>` while git still lists the worktree, and `Delete branch <name>`
  while the branch exists. The blocks are read in the background and say `checking…` until
  git answers; a block whose repository is busy with another branch change says so. Neither
  shows a box, and deleting while a block is still checking removes nothing for it. A box
  checked on a block whose repository turned busy meanwhile removes nothing and is reported
  as kept. A block with nothing left to remove is hidden.
- The sheet warns before you check anything: the worktree box says how many uncommitted
  files the worktree has, and the branch box says `not merged` when the branch is not merged
  into the base it started from. A branch that is checked out in another worktree cannot be
  checked, nor one still held by a worktree whose folder is gone (`its worktree folder is
  gone; run git worktree prune`), and the branch of a `Worktree` session can only go
  together with its worktree.
- Checked boxes are forced: the worktree is removed with `git worktree remove --force`, so
  its uncommitted files are lost, and the branch with `git branch -D`, so unmerged commits
  are lost. The main checkout is never removed.
- The removal runs in the background after the sheet closed; the project row says
  `cleaning up…` meanwhile. Each repository is read again first, and a worktree or branch that
  no longer is what the sheet showed is kept and reported: a worktree git no longer lists, with
  something else checked out or with another number of uncommitted files, a branch that is
  gone, checked out elsewhere, or merged when the sheet showed it so and no longer. Two limits
  remain: a branch the sheet showed as not merged is deleted even if more commits arrived on
  it since, and the agent of a running session may still write into its worktree between
  the fresh reading and the removal.
- A removal that fails or keeps something is reported in the error bar. The session is
  deleted regardless.
- Deleting removes Partitur's records of the sessions and their transcripts, so the Session
  and Env tabs have nothing left to show. The agent's own session data, such as Claude Code's
  files under its config folder, is left alone.
- It cannot be undone. To get a session out of sight and keep it, archive it instead.

Partitur does not delete anything on its own, and it removes a worktree or branch only for a
box you checked.
