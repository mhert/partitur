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
  after the first prompt, and Partitur takes it. The agent may also rename its session with
  Partitur's `rename_session` tool, which replaces any title, also one you typed; a child
  proposes a title to its orchestrator instead (see [Orchestration](orchestration.md)). A title
  you typed, one an agent set with `rename_session`, or one an orchestrator gave a child, is
  never replaced by Claude Code's title; a child whose title the split card derived from its
  prompt and you left alone takes the agent's title. The first prompt is kept as it was typed, with
  the skill it starts with, and does not change the title.

## Attaching files

- The `+` button beside the prompt box picks files to send with the next prompt.
- `Ctrl V` (`Cmd V` on macOS) in the prompt box attaches files copied in a file manager, and
  text made only of `file://` lines. A copied image or screenshot is saved as a PNG and
  attached; a `pasting…` chip shows until it is saved, and an image that cannot be saved is
  reported in the error bar. Other copied text is pasted into the prompt.
- Copied files and images come before text: text copied in an office program that also puts
  a picture of it on the clipboard is attached as that picture. `Ctrl Shift V` (`Cmd Shift V`)
  pastes the copied text instead.
- Until a paste has landed, the prompt is not sent: `Enter` does nothing and the send button
  is disabled.
- Pasted images are saved in `partitur-paste` in the user's runtime folder
  (`XDG_RUNTIME_DIR`), or else in the system's temporary folder. On Linux and macOS that
  folder is created for you alone, and one that other users can reach is refused.
- Each attachment is a chip above the prompt box, with a button that removes it.
- PNG, JPEG, GIF and WebP files go to the agent inline as images; any other file goes as a
  link the agent reads itself.
- A sent prompt shows a preview of each image and the names of its other files. A click on a
  preview opens the full picture over the window, scaled down to fit and never enlarged
  (`Escape` or a click beside it closes it).
- Images sent are kept with the session as a copy and a preview, and are deleted with it.
- A prompt with attachments is prepared before it goes out: its images are copied and the
  composer says `sending…` until it is sent, or until the error bar says why it was not.

## Widgets

Some MCP tools come with a widget (an MCP App): a small page that shows the tool's result,
such as a Jira issue card. Partitur shows it in the transcript, right under the tool call's
`tools` row. In an expanded row, each call's widget sits under the call's own line.

- The widget takes the transcript's width. Its height is the one the widget asks for, kept
  between 80 and 640 pixels; until it asks, it is 160 pixels tall. The widget is told its
  width, again whenever it changes, and that 640 pixels is the most it gets.
- While the widget's page is being read, and until its first picture is ready, the place
  says `Loading widget…`.
- Widgets are read-only. They see the tool's input and result, and the pointer works in
  them, but they take no keys and cannot call tools or send messages into the session.
- Links open in the system's browser, never inside the widget. A link you click in a widget,
  one that opens a new window included, and a link the widget asks to open go to the
  session's [`link_opener`](configuration.md#settings) when it is an `http`, `https` or
  `mailto` link; other links open nothing. The widget stays as it was. A widget that tries
  to leave its page without a click is just kept where it is.
- While the pointer is over a widget, the wheel and the touchpad scroll the widget, not the
  transcript, also right after the transcript scrolled the widget under the pointer, with
  the pointer at rest, from the very next turn. Everywhere else they scroll the transcript. While a mouse button is held, as when you
  select text, a wheel that is scrolling the transcript goes on scrolling it. Over a widget
  the pointer looks as the widget asks, such as a hand over a link or an I-beam over text.
- Each widget runs in a web origin of its own, apart from Partitur and from every other
  widget. It may use storage there as a web page does, but a widget opened again starts
  with none. Like any page embedded from another site, it keeps no cookies.
- A widget follows the desktop's light or dark preference and switches when it changes.
- The widget is kept with the session, so an old session shows its widgets again without the
  MCP server connected. Switching to another session closes the widgets of the one before,
  and coming back opens them again. A widget scrolled out of sight, or on a tab not shown,
  stops painting but keeps its state.
- A widget that cannot be shown leaves the tool call as it is without one, with its text in
  the expanded call, and adds one muted line under it: `Widget unavailable: <reason>`. That
  happens when its page could not be read, when the widget engine is not available or fails,
  and for a widget that was still loading when Partitur closed (`the widget was still
  loading when the session closed`).
- Widgets are drawn by an embedded Chromium (CEF), which starts in the background with
  Partitur. When it cannot start, every widget falls back this way with `widget host
  unavailable: widgets are not available`. On macOS it is not supported yet, so there every
  widget falls back.

## Interrupted sessions

When Partitur closes while a session runs or waits for you to answer a permission request,
that turn never ends. On the next start the session shows `interrupted`, and its transcript
says `Partitur closed before this turn ended`. The mark stays until the session's next turn:
a prompt resumes the session as it would an idle one. A session whose turn finished or failed
is not interrupted, and neither is an orchestrator whose turn ended on a proposal you had not
decided yet. A session whose turn ended at a [workflow gate](workflows.md#gates) still asks
you after the restart, with its gate open; an orchestrator's child waits for its orchestrator
again instead. A stop counts only once the agent confirmed it: quitting right after pressing
stop can still leave the session interrupted.

## Background work

Claude Code can leave work running in the background after its reply ends: subagents,
commands it sent to the background, monitors and other tasks. While any run, the session's
pill reads `1 background task` or `N background tasks` in the running colour instead of
`done` (or `idle`), in the tree, the jump panel, the session header, a parent's child cards
and the archive and delete sheets. The pill turns to `done` when the last one ends.

- The session takes prompts at once: its turn is over, and a prompt does not wait for the
  background work.
- It counts as working: a parent shows `running`, it sorts with the running sessions, and a
  parent does not count it among its children done. A command that never ends on its own,
  such as a dev server or a watcher, keeps the session working for as long as it runs.
- Stopping the session's turn also stops its subagents. Background commands and monitors
  keep running: Claude Code spares them on purpose, and Partitur has no Stop for them yet.
- The count lives in memory only. It drops to 0 when the agent's connection closes or the
  session moves to another backend, and a restart starts without it, since the background
  work ends with Partitur. A failed turn keeps the count but shows `failed`.

## Questions

An agent that wants you to choose between named options asks through Partitur's `ask_user`
tool instead of writing the options out, and ends its turn. The question is a card in the
transcript where it was asked: the header `Question`, the question, then one row per option
with the key that picks it (`1` to `6`), its label and its description, if it has one. Under
the options sit the `Other` field with `Send`, and `Cancel`.

- Clicking an option, or pressing its digit with the cursor in no field, sends the option's
  label as your next prompt.
- `Other` sends what you typed, trimmed, as your next prompt, with `Send` or `Enter` in the
  field. Blank text sends nothing.
- `Cancel`, or `Escape` on the Session tab, closes the card and puts the cursor into the
  prompt box. Nothing is sent to the agent.
- A prompt you send through the prompt box while the card is open also closes it, as the
  answer.
- While the session is in its turn the options and `Send` take no click, and the digits do
  nothing; `Cancel` still works.

A closed card stays in the transcript, dimmed, and says how it closed: `answered: <text>`,
`answered by orchestrator: <text>` when a child's orchestrator answered it, `cancelled`, or
`replaced by a newer question` when the agent asked again before you answered. A session
whose turn ends with a question open `asks you` in the tree, and still does after a restart,
with its card open. An orchestrator's child asks its orchestrator instead: it reads
`waits for orchestrator`, and the orchestrator answers it (see
[Orchestration](orchestration.md)); you may still answer its card yourself. After `Cancel`, a
session that waited only for the question is `done`. While a question is open the session
takes no prompt Partitur generated for it, such as an orchestrator's follow-up; answering
sends it after the answer's turn, and `Cancel` sends it at once.

## The Env tab

The Env tab shows what shaped the selected session: the `Instructions` that apply to it, its
`Backend` (backend, command, folder, model and effort) and how much of its `Context` it has
used. A session that runs in a repository also has a `Git` card, between those and `Context`:

- `repository` names the repository, and `workspace` says how the session uses it:
  `checked out`, `new branch`, or `worktree <folder>`, with the home folder written as `~`.
- `made on` gives the branch the session was made with and its base (`feat/x from main`,
  plus `, fetched` when the base was fetched first). A `checked out` session made no branch,
  so the row reads `first seen on` and gives the first branch Partitur saw it on instead.
- `now` is what git reads in the session's folder: `<branch> · <n> uncommitted`, or
  `(detached at <commit>)` instead of a branch. It says `checking…` while git reads, and
  `git could not read <folder>: <reason>` when the read fails, for example when the
  worktree's folder is gone. A git command that runs longer than 30 seconds is stopped and
  the read fails with that reason, so `checking…` never stays for good.
- A warning line `now on <current>, made on <expected>` (or `first seen on`) appears when the
  session is on another branch than that one.
- A warning line `touched <n> files outside its folder: <first path>…` appears when the
  agent's tool calls read or wrote files outside the session's folder, such as in another
  worktree. The session's folder counts in both the spelling Partitur recorded and the one git
  resolves it to (through a symlink, say) once git has read it, and keeps both while the session
  is archived and after a restart. On Windows, paths that differ only in letter case count as
  the same. Files in the temporary folder, in a hidden folder directly in your home folder
  (where agents keep their configuration and memory), in the session's attachment folder,
  and files you attached to a prompt do not count. It comes from the transcript, so it is
  still there after a restart.

Git is read in the background whenever the Env tab opens, and again each time a turn of the
shown session ends (also when its agent crashed) or the shown session is restored while the tab
is open. Partitur only reports a branch change or files
outside the folder; it stops nothing. An archived session is not read, so its `now` row says
`not checked`.

## Archiving sessions

A finished session can leave the tree without losing anything. The highlighted row, and the
row under the pointer, end in an `Archive` button; `A` does the same for the highlighted row.
A sheet asks for confirmation first, and only on confirmation does anything change.

- `Done`, `Failed`, `Interrupted` and idle sessions archive directly. A session that runs,
  waits for you or still runs background work offers `Stop and archive`: confirming stops
  its turn and its subagents first, the way the stop button does.
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

- A session that runs, waits for you or still runs background work offers `Stop and delete`:
  confirming ends its turn and its background work as its connection closes.
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
- Deleting removes Partitur's records of the sessions and their transcripts, so the Session,
  Spec and Env tabs have nothing left to show: they are disabled and the Projects tab opens.
  The agent's own session data, such as Claude Code's files under its config folder, is left
  alone.
- It cannot be undone. To get a session out of sight and keep it, archive it instead.

Partitur does not delete anything on its own, and it removes a worktree or branch only for a
box you checked.
