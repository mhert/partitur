# Sessions

## Opening a project

`open project folder` at the end of the tree, or `PARTITUR_OPEN=<folder>` at the start, opens
a folder as a project. The folder is scanned for its repositories in the background: until the
scan and the project's `settings.toml` are read, the tree shows the folder's name and path
with a dim `scanning…`, and picking the same folder again meanwhile changes nothing. A folder
already known, by any spelling of its path, is rescanned and reopened with its sessions. A
folder that cannot be opened leaves the tree and the error bar says why.

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
  you picked stay, but for the `Existing` branch picked in the `Branch & worktree` box, which
  belonged to the repository before. The subline reads
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
- The `Branch & worktree` box decides how the session uses git with two switches. The
  `folder` is `Main checkout` or `Worktree`; the `branch` is `As is`, `Existing` or `New`.
  Every segment is always shown, and one the session is not offered is disabled.
  - `Main checkout` + `As is` works in the repository as it is now and runs no git command.
    The box says which branch that is in its `checked out` row (`(detached)` without one) and
    notes `works in <repo> as it is now`.
  - `Main checkout` + `New` creates a branch and switches the repository itself to it;
    uncommitted changes come along, as with `git switch -c`.
  - `Main checkout` + `Existing` switches the repository itself to a branch that exists
    (`switches <repo> itself to <branch>`). Picking the branch the repository has already is
    the same as `As is`.
  - `Worktree` + `New` creates the branch in a new worktree and runs the agent there; the box
    shows the folder under `worktree at`, with the home folder written as `~`.
  - `Worktree` + `Existing` runs the agent in a worktree on a branch that exists: in the
    worktree the branch already has (`uses its existing worktree`, no git command), or in a
    new one made for it (`adds a worktree for it`, placed by the worktree pattern). The path
    shows in the `worktree at` row; the notes do not repeat it.
  - `As is` is not offered in a worktree: picking `Worktree` while `As is` is selected moves
    the branch choice to `New`.
- `name` and `base branch` apply to `New`. The name is trimmed and must be a valid git branch
  name that does not exist in the repository yet. The base is picked from the repository's
  local branches. A new Worktree must not exist as a folder already. What is wrong shows under
  the box and keeps `Create session` from working: `enter a branch name`,
  `"<name>" is not a valid git branch name`, `branch <name> already exists in <repo>`,
  `<path> already exists`. Whether the Worktree's
  folder exists is looked up in the background each time the box changes, so `<path> already
  exists` shows a moment after the name is typed. `Create session` looks again when pressed
  and waits for that answer, so a folder made meanwhile still refuses it. The split card of
  an orchestrator checks its children's worktree folders the same way, `Approve split`
  included.
- `Existing` shows an `existing` picker, a type-ahead over the repository's local branches (no
  base and no fetch row). Typing filters the list and `Enter` picks the first branch left. It
  is one list: the branches checked out in a worktree come first, each with its folder, and
  the other branches follow, the one the main checkout has marked `checked out`. Branches that
  exist only on a remote are not listed, and nothing is fetched. What is wrong shows under the
  box, the first that applies: `pick a branch`, `<name> is not a branch of <repo>`, `<branch>
  is checked out in <path> — pick Worktree` (in `Main checkout`, for a branch another worktree
  has; git would refuse the switch), `<branch> is checked out in <repo>'s main checkout — pick
  Main checkout` (in `Worktree`), `its worktree at <path> is gone; run git worktree prune`
  (the branch's only worktree has no folder any more; Partitur does not prune), `<repo> is in
  use by "<title>" — use a worktree` (a switch of the main checkout while a live session works
  there), `<path> already exists` (for a new worktree) and `<repo> is busy with another branch
  change`.
- When a new branch's name is one that exists, the box says `branch <name> already exists in
  <repo>` as before and a `use existing` link beside it. Clicking it changes the branch choice
  to `Existing` with that branch picked and keeps the folder choice. The link appears only
  there; a name that exists is never turned into `Existing` by itself.
- Sessions share folders and branches. Any number of sessions may work in one folder (the
  main checkout or a worktree) and on one branch, so a branch with a worktree that another
  session works in is not refused: the note ends ` · also used by "<title>"`, or
  ` · also used by "<title>" and <n> more`. The note names every session that is not deleted,
  archived ones included, since restoring them must find their folder. On the split card of
  an orchestrator the other children are named first.
- `Fetch <remote> and start from <remote>/<base>` fetches the base's upstream remote (else
  `origin`) first, if that remote has the base, and starts the branch from `<remote>/<base>`.
  The local base branch is never moved. Without such a remote branch the box says
  `<base> has no remote counterpart` and the fetch cannot be checked. A fetch that takes more
  than a minute is stopped and fails the create.
- The box starts from the project's newest session: its folder and branch choice and fetch
  choice, where `Existing` is remembered as `New`, since a picked branch belongs to one
  session. Without a session it starts `Main checkout` and `As is` with fetching on. The name
  always starts empty, no existing branch is picked, and the base starts at the branch checked
  out now.
- Switching the main checkout's branch (`New` or `Existing` in `Main checkout`) is refused
  while a live (not archived) session works in it, with `<repo> is in use by "<session title>"
  — use a worktree`, since switching the branch would pull it from under that session. Only
  live sessions count for this refusal.
- Git runs in the background. The branches are read when the sheet opens (the rows say
  `loading…`, and `reading the branches of <repo>…` stops `Create session` meanwhile;
  `Main checkout` with `As is` can be created at once). Reusing an existing worktree, or
  picking the branch the repository has already, needs no git work, and `Create session`
  creates the session at once. Otherwise it does the git work (a new branch, the switch to an
  existing one, a new worktree) in the background: the sheet freezes, the button says
  `creating…` and `Cancel` stays available. The session is created only once git succeeded;
  when a step fails, nothing is created and the sheet says why. `Cancel` meanwhile closes the
  sheet and undoes what git made (the worktree and branch, or the switch back to the previous
  branch or detached head). Only what was made is undone: a branch or worktree that existed
  before is never removed.
- Only one changing git request runs per repository at a time. While another one (including
  the undo of a cancelled create) is still running there, every choice that needs git work for
  that repository is refused with `<repo> is busy with another branch change`. Partitur also
  runs every change to one repository after the one before, whatever path names it, so a
  cleanup and a create there never collide on git's own locks; reading git never waits.
- A project folder that is not a repository offers only `Main checkout` with `As is`, and says
  so. So does a repository whose git state cannot be read.
- On an orchestrator's sheet the box sets the defaults for its children. It offers the folder
  choice and the branch choices `As is` and `New` only: `Existing` is picked per child on the
  split card, since one branch would mean nothing in the other repositories. `As is` means
  `children work in their repositories as they are now`; `New` shows the common base branch
  (the branches every repository has; `per child` when there are none), whether to `Fetch
  first and start from each repository's remote base`, and a branch name that may stay empty
  because each child can be named on the split card. There is no `worktree at` line.
- Git work still running when the window closes finishes, but no session is recorded for it.
- A session is titled `untitled` until the agent titles it: Claude Code sends a short title
  after the first prompt, and Partitur takes it, as it takes GitHub Copilot's when Copilot
  sends one. The agent may also rename its session with Partitur's `rename_session` tool,
  which replaces any title, also one you typed; a child proposes a title to its orchestrator
  instead (see [Orchestration](orchestration.md)). A title you typed, one an agent set with
  `rename_session`, or one an orchestrator gave a child, is never replaced by the agent's own
  title; a child whose title the split card derived from its prompt and you left alone takes
  the agent's title. The first prompt is kept as it was typed, with the skill it starts with,
  and does not change the title.
- A session's history loads in the background when it is first opened after a start: its
  transcript says `Loading…` until it is read, and a prompt you send meanwhile goes out once
  it is. A history that cannot be read says so in its place, in the error colour, with the
  error in the error bar; selecting the session again tries once more.
- The first prompt an agent gets carries the `AGENTS.md` instruction files that apply (and an
  orchestrator's introduction). Partitur reads those files in the background when you send
  it, so it goes out a moment later, with the files as they are then.

## Sessions an agent starts

A plain session (neither an orchestrator nor a child) may propose a new session of its
project with Partitur's `propose_session(title, prompt, repository?, base?)` tool, for work
that should run on its own, such as a hand-off. The tool is not offered to an orchestrator or
a child. A blank prompt is refused (`propose_session needs a prompt`), as is a repository that
is not one of the project's. `repository` defaults to the session's own, or the repository an
orchestrator would work on for a session of the whole project; a project without any is
refused (`this project has no repository to start a session in`). A blank title is derived
from the prompt. The call answers `proposal recorded, awaiting approval` at once, and the
agent ends its turn.

- The proposal appears in the proposer's transcript as the one-row `Proposed session` card,
  headed `1 new session`. It is the card of [Orchestration](orchestration.md) with one row:
  there are no `skipped` rows, no `Edit split`, no picker that adds a row and no button that
  removes it. You edit the row's title, prompt, folder and branch choice, branch name or
  existing branch, base and fetch choice as on a split card, and reject the proposal instead
  of removing the row. Its button reads `Approve` (or `Enter`).
- The row starts from the proposer's own branch settings: its folder and branch choice
  (`Existing` as `New`) and fetch choice, no branch name, and the base the agent passed as
  `base`. Without one the proposer's base is used when the new session is in the proposer's
  repository, and the checked-out branch otherwise. A `base` that is not a branch of the
  repository is not refused: the card then starts from the checked-out branch. In the
  proposer's repository the base picker marks the branch the proposer works on `(proposer's
  branch)`. `Main checkout` always means the repository's main checkout, never the proposer's
  worktree. Nothing on the card says `from orchestrator`.
- While the proposal is undecided the proposer asks you, as an orchestrator does for a split,
  and shows that in the tree.
- Approving starts the session at the top level of the tree, not as a child of the proposer,
  and its first prompt starts with a note naming the proposer, then a blank line and the
  prompt:
  `Started by session "<title>" (<id>). Read it with read_session when you need its context.`
  The proposer is woken with a `session started:` prompt that lists the session like a
  started child (`- <title> · <repo> · session <id> · <mode> <branch> · in <folder>`) and
  ends `It runs on its own and reports nothing back; read it with read_session(session_id).`
  The new session never reports back to the proposer. The card then shows `1 session started`
  and its row opens the session (`click the row to open the session`).
- Rejecting wakes the proposer with `proposal rejected by the user`, followed by your note on
  a line of its own (`note: <text>`) when you wrote one.

## Reading other sessions

Every session, whatever its role, has two tools to look at the sessions of its own project:

- `list_sessions()` answers `sessions:` and one line per session of the project: the live
  ones newest first, then the archived ones newest first. A line reads
  `- <title> · <repository> · session <id> · <status>`. The second field is the repository
  the session works in, `orchestrator` for an orchestrator and `project` for any other
  session of the whole project. The status is `idle`, `running`, `waiting for you`,
  `interrupted`, `done` or `failed`. Further fields follow when they apply, in this order:
  the workflow step, `asks a question` for an open question or else `waits at its <gate>` for
  an open gate, `child of <orchestrator id>`, `archived`, and `this session` on the caller's
  own line.
- `read_session(session_id, page?)` returns a session's conversation as text. A session of
  another project, or one that does not exist, is refused (`<id> is not a session of this
  project`). The text is cut into pages of about 20,000 characters, oldest first; an entry is
  never split across pages, and one longer than a page is cut to a page ending in `…`. Without
  `page` the last page is returned, so the newest part comes first. Each page starts with
  `session <id> · <title> · page <n> of <m>`, and every page but the first ends with
  `earlier: read_session(session_id, page: <n-1>)`. A page outside the range is refused
  (`page <n> is out of range; the session has pages 1 to <m>`). A session without entries to
  show has the one page `(no entries yet)`.
- The entries are separated by a blank line, each in one kind of line: `user: <text>` (with
  `[attachments: <names>]` after files), `agent: <text>`, `tool: <title> · <status>`,
  `permission: <title> · <outcome>` (`open` while undecided, `cancelled` when dismissed, otherwise the name of the option chosen), `notice: <text>`, `turn failed: <text>`,
  `proposed: <title> (<repo>), … · undecided`, `approved` or `rejected`,
  `question: <text>`, `orchestrator: <text>`, `partitur: <text>`,
  `child result · <repo> · <title>: <text>`, `sent to child <id>: <prompt>`,
  `title proposal: <new> (was <old>)` and `workflow: <event>`, where the event is written as JSON. Thoughts, turn markers, tool
  output and widget content are left out.
- A session you have not opened since Partitur started is read from its stored history in the
  background, with everything written to it before the call, without loading it into the
  window; the answer comes when it is read.

Calls to these tools are allowed without asking.

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
- A sent prompt shows a preview of each image and the names of its other files. An image
  shows its name while its preview loads, which never holds up the window, and when it has
  none. A click on a preview opens the full picture over the window, scaled down to fit and
  never enlarged (`Escape` or a click beside it closes it).
- Images sent are kept with the session as a copy and a preview, and are deleted with it.
- A prompt with attachments is prepared before it goes out: its images are copied and the
  composer says `sending…` until it is sent, or until the error bar says why it was not.

## Widgets

Some MCP tools come with a widget (an MCP App): a small page that shows the tool's result,
such as a Jira issue card. Partitur shows it in the transcript, right under the tool call's
`tools` row. In an expanded row, each call's widget sits under the call's own line.

- The widget sits in a frame like the tool call's row: the same border, corners and padding,
  in the light and the dark theme. Inside the frame it takes the transcript's width. Its
  height, the frame's border and padding not counted, is the one the widget asks for, kept
  between 80 and 640 pixels; until it asks, it is 160 pixels tall. The widget is told its
  width, again whenever it changes, and that 640 pixels is the most it gets.
- While the widget's page is being read, and until its first picture is ready, the place
  says `Loading widget…`, in the same frame.
- Widgets are read-only. They see the tool's input and result, and the pointer works in
  them, but they take no keys and cannot call tools or send messages into the session.
- Links open in the system's browser, never inside the widget. A link you click in a widget,
  one that opens a new window included, and a link the widget asks to open go to the
  session's [`link_opener`](configuration.md#settings) when it is an `http`, `https` or
  `mailto` link; other links open nothing. The widget stays as it was. A widget that tries
  to leave its page without a click is just kept where it is.
- A widget opens links only when you click it: each click lets one link through, within two
  seconds of the press or the release. A widget that asks to open links on its own, or many
  on one click, opens no more than that.
- While the pointer is over a widget that can scroll, the wheel and the touchpad scroll the
  widget, not the transcript, also right after the transcript scrolled the widget under the
  pointer, with the pointer at rest, from the very next turn. A widget can scroll when its
  content is taller than its frame, or when it never said how tall it is. Over a widget
  whose content fits, and everywhere else, they scroll the transcript. While a mouse button
  is held, as when you select text, a wheel that is scrolling the transcript goes on
  scrolling it. Over a widget
  the pointer looks as the widget asks, such as a hand over a link or an I-beam over text.
- Each widget runs in a web origin of its own, apart from Partitur and from every other
  widget. It may use storage there as a web page does, but a widget opened again starts
  with none. Like any page embedded from another site, it keeps no cookies.
- A widget loads scripts, styles, pictures and fonts, connects and nests frames only from
  the origins its MCP server declares for it, under the policy the MCP Apps specification
  builds from them. Only web origins count (`http`, `https`, `ws` and `wss`, also
  `https://*.example.com` for every subdomain); `*`, a scheme alone and every other entry are
  ignored.
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
- Chromium runs its widget processes in the operating system's sandbox where it can. On Linux
  Partitur first checks that the sandbox starts, and without it Chromium runs unsandboxed and
  Partitur's log says why (`CEF runs without the process sandbox: …`). On Windows Chromium
  always runs without it for now, with the same line in the log: its sandbox needs a start-up
  executable Partitur does not ship yet. Each widget is still kept to its own origin, its
  frame and the requests and navigations allowed above.

## Drawings

An agent can show you something it drew: an SVG (a diagram, a sketch, a chart) or an HTML
page. It does so with the `show_svg` tool, which every session has, and the `show_html` tool,
which a session has where widgets run, and goes on without waiting. Nothing comes back from the drawing to the agent.

- The drawing opens over the window, in the viewer that also shows a picture you sent, when
  the agent is in the session you look at. An agent in another session opens nothing: the
  drawing is kept in its transcript and waits for you there, and the agent is told so.
- The call stays in the transcript as a line in the `tools` row, `Drawing` for an SVG and
  `Page` for an HTML page, with an `Open` button that shows it again. This works in an old
  session and after a restart too. A click on the line itself still expands its details.
- Escape or a click beside the drawing closes the viewer. A new drawing while the viewer is
  open replaces the one it shows.
- An SVG sits on a white canvas in the light and the dark theme, since a typical drawing has
  dark lines and no background of its own. An SVG with a width and a height of its own is
  shown at that size when it fits inside the viewer, and scaled down otherwise; one without
  them, as most drawings are, fills the viewer keeping its ratio. A picture inside an SVG
  shows only when it is embedded as a `data:` URL; one it names by a file path or a web
  address is left out. An SVG that cannot be read shows `cannot show the drawing:` and the
  reason.
- An HTML page fills the viewer and follows the desktop's light or dark preference through
  the `prefers-color-scheme` media query, switching when the preference changes. Until its
  first picture is ready the viewer says `loading…`. The pointer and the wheel work in the
  page, keys do not.
- A page is kept to what a widget is kept to: it loads nothing from the network, only inline
  scripts and styles and `data:` pictures and fonts, it cannot navigate away or open windows,
  and a link opens in the system's browser, per the `link_opener` setting, only on a click.
- Pages are drawn by the same embedded Chromium as [widgets](#widgets), so where widgets are
  not available (macOS for now) the agent is not offered `show_html`; SVG works everywhere.
- A drawing may hold up to 2 MB of markup. An empty or a larger one is refused and the agent
  is told why.

## Permission requests

A session that asks you to allow a tool call shows `waiting for you` until you answer.

- A request still open when the agent's turn ends stays open, and the session keeps waiting
  for you. A request that arrives after the turn ended, as a subagent's can, makes the
  session wait for you too.
- Once every open request is answered, the session goes back to the status it had: `done`,
  or `waiting for you` while a proposal, a [workflow gate](workflows.md#gates) or a
  [question](#questions) is open at that moment. A prompt Partitur generated for it meanwhile,
  such as an orchestrator's follow-up, waits until then and goes out after the answer, unless
  the session's last turn was stopped or cancelled: then it waits until you prompt the
  session. A request answered inside a running turn lets the session run on, and so does one
  that waited when the agent started a turn of its own.
- When the agent's connection closes while such a request is open, the request is dropped,
  the session takes the status it would have gone back to, and a held prompt goes out as
  after an answer.
- `Stop` cancels the open requests. A turn the agent ends as cancelled cancels them too, so a
  subagent still waiting on one goes on; a turn that fails drops them, since the agent can no
  longer receive an answer.

## Interrupted sessions

When Partitur closes while a session runs or waits for you to answer a permission request,
that turn never ends. On the next start the session shows `interrupted`, and its transcript
says `Partitur closed before this turn ended`. Like a session that asks you, it sorts first
among its siblings, marks every session above it with `child asks you`, so a folded parent
still shows it, and the status line counts it as `N interrupted`. The mark stays until the
session's next turn: a prompt resumes the session as it would an idle one. A session whose turn finished or failed
is not interrupted, and neither is an orchestrator whose turn ended on a proposal you had not
decided yet. A session whose turn finished while a [permission
request](#permission-requests) stayed open, or that got one after its turn, does come back
interrupted: the request is not kept across the restart. A session whose turn ended at a
[workflow gate](workflows.md#gates) still asks
you after the restart, with its gate open; an orchestrator's child waits for its orchestrator
again instead. A stop counts only once the agent confirmed it: quitting right after pressing
stop can still leave the session interrupted.

## Failed sessions

A session whose turn failed, whose agent connection closed during a turn, or that could not
be created shows `failed`, and its transcript says why. Like an interrupted session it needs
you: it sorts first among its siblings, marks every session above it with `child asks you`,
so a folded parent still shows it, and the status line counts it, with its dot, as
`N failed`. The mark stays until the session's next turn.

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

GitHub Copilot reports no single tasks, so on Copilot the background work counts as one
(`1 background task`) from the moment its reply ends until Copilot says the whole session is
quiet. A prompt sent meanwhile waits for that, as described below.

## GitHub Copilot

A session on a [Copilot backend](configuration.md#github-copilot) works like one on Claude
Code, with these differences:

- The turn ends when Copilot says its reply is done, not when it answers the prompt, which it
  does only once the background work has ended too. A turn Copilot reports as aborted ends
  cancelled.
- Copilot cancels a prompt that is still unanswered when the next one arrives. Partitur
  therefore holds a prompt sent while an earlier one is unanswered, background work included,
  and sends it once that one is answered; the session shows its turn as started meanwhile.
  `Stop` drops every held prompt, ends their turns as cancelled, and stops the turn Copilot
  is in. Copilot stops its own subagents.
- Subagents run inside the session, and what they write and call is not shown; their result
  reaches the transcript through the session's own reply.
- The session is switched to Copilot's asking mode (`agent`) with `allow_all` off as it opens,
  so permission requests are asked as on Claude Code (or granted by `Auto-allow tool calls`).
  Copilot asks for every edit, every shell command it does not know to be read-only and every
  MCP tool call. A Copilot that offers neither setting leaves the session with the notice
  `this agent offers no default (ask) mode; permissions follow the agent's own policy`.
- A tool call shows Copilot's title for it. A call of one of Partitur's own tools, which
  Copilot titles `partitur-<tool>`, is treated as that tool, so it is allowed without asking as
  on Claude Code.
- Model, Effort, slash commands and skills come from Copilot, as they come from Claude Code.
- Copilot shows no [widgets](#widgets): its tool calls show their text.

## opencode

A session on an [opencode backend](configuration.md#opencode) works like one on Claude Code,
with these differences:

- opencode is kept asking by the configuration Partitur hands it at start, not by a mode
  switched in the session, so no notice says it is not kept asking. An orchestrator's editing,
  commands and subagents are denied by that configuration too, and its opencode is not started
  again.
- The turn ends when opencode answers the prompt. After a command it ran in the background or
  a subagent (every opencode subagent runs in the background) finishes, opencode carries on
  with a further turn of its own, which nothing marks the end of: it shows in the transcript as
  it comes, with no working indicator. On a child, that late text is not part of the result
  sent back to the orchestrator, which is the child's last message when its turn ended.
- In a plain session opencode asks before a subagent starts. What the subagent then does is
  not shown, but its permission requests are asked in the session that started it, also after
  that session's turn has ended: the session then waits for you until you answer, and goes
  back to its status after that (see [Permission requests](#permission-requests)).
- Rejecting a permission ends opencode's turn as cancelled; that is opencode's own way. A
  rejected Code Mode `execute` is the exception: the model sees it as a failed call and goes on.
- While opencode retries a model it cannot reach, the session shows a notice for each attempt,
  with the error; once opencode gives up, the turn fails with it.
- A call of one of Partitur's own tools, which opencode names `partitur_<tool>`, is treated as
  that tool. Every other tool but those that only read asks, an MCP tool of your own and Code
  Mode's `execute` among them.
- Model, Effort and Session Mode (`build`, `plan`) come from opencode. opencode shows no
  [widgets](#widgets): its tool calls show their text.

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
used. The instruction files are read in the background when the session's agent starts and
each time the Env tab opens; until the first read arrives the card says `reading AGENTS.md…`.
A session that runs in a repository also has a `Git` card, between those and `Context`. A
project session has one when the project folder is itself a repository:

- `repository` names the repository, and `workspace` says how the session uses it:
  `checked out`, `new branch`, `existing branch`, `worktree <folder>`,
  `worktree <folder> · existing branch` or `existing worktree <folder>`, with the home folder
  written as `~`.
- `made on` gives the branch the session was made with and its base (`feat/x from main`,
  plus `, fetched` when the base was fetched first). A `checked out` session made no branch,
  so the row reads `first seen on` and gives the first branch Partitur saw it on instead.
- `now` is what git reads in the session's folder: `<branch> · <n> uncommitted`, or
  `(detached at <commit>)` instead of a branch. It says `checking…` while git reads, and
  `git could not read <folder>: <reason>` when the read fails, for example when the
  worktree's folder is gone. A git command that runs longer than 30 seconds is stopped,
  together with every program it started, and the read fails with that reason, so
  `checking…` never stays for good.
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
- For every session of the family that made a worktree or a branch, the sheet shows a block
  headed `<repository> · <title>` with two boxes, both unchecked: `Remove worktree <path>`
  while git still lists the worktree, and `Delete branch <name>` while the branch exists. Only
  what the session made is offered: a `Worktree` + `New` session offers both, a `Main
  checkout` + `New` session its branch, and a session that added a worktree for an existing
  branch only that worktree. A session that reused an existing branch or worktree, or works in
  the repository as it was, made nothing and has no block. The blocks are read in the
  background and say `checking…` until git answers; a block whose repository is busy with
  another branch change says so. Neither shows a box, and deleting while a block is still
  checking removes nothing for it. A box checked on a block whose repository turned busy
  meanwhile removes nothing and is reported as kept. A block with nothing left to remove is
  hidden.
- A worktree or branch that a session outside the family uses cannot be removed. Every other
  session of the project counts, archived ones included: one whose folder is the worktree
  holds it, one that works on the branch holds the branch. The box is disabled with the note
  `in use by "<title>"`, or `in use by "<title>" and <n> more`. This is checked again when the
  removal is queued and after git was read afresh, so a session that began using the worktree
  or branch after the sheet opened still keeps it, and that is reported.
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
