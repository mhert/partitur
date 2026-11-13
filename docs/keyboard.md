# Keyboard

- `Tab` switches to the next tab, skipping the Spec tab while it is disabled; `Ctrl 1` to
  `Ctrl 4` select Projects, Session, Spec and Env. The Spec tab is enabled for a session that
  reports a [workflow](workflows.md).
- `G` opens the Spec tab of the session that has waited longest at a gate, on every tab; it
  does nothing when no session waits.
- `Ctrl K` opens the jump panel: type to filter the sessions of all open projects, move with
  the arrow keys, `Enter` opens the highlighted session, `Escape` closes the panel. Archived
  sessions are listed only while its `include archived` box is checked.
- On the Projects tab the arrow keys move the highlighted row, skipping the rows of a folded
  project, `Enter` opens its session, `N` opens the new-session sheet for its project, `A`
  archives its session and `D` deletes it.
  After folding the project that held the highlighted row, `N` opens the sheet for that
  project until a row is highlighted again. The status line names `Enter`, `G`, `N` and `A`.
- On the Projects tab `F2`, or a double click on a session's title, edits the title: `Enter`
  keeps what you typed (a blank title changes nothing) and `Escape` keeps the old one. While
  the title field is open no other key reaches the tree. A single click on a title only
  highlights its row; a click elsewhere on the row opens the session.
- The chevron in a project header folds the project's rows away under the header and opens
  them again; Partitur remembers which projects are folded. Opening a session from elsewhere
  (the jump panel, a card) opens its folded project. The header's close button `×` shows
  only while the pointer is over the header.
- In the archive sheet `Enter` archives and `Escape` cancels, and in the delete sheet `Enter`
  deletes and `Escape` cancels; no other key reaches the window while either is open.
- In the new-session sheet `Enter` creates the session and `Escape` cancels; no other key
  reaches the window while it is open.
- In the prompt `Enter` sends and `Shift Enter` breaks the line.
- On the Session tab, with the cursor in no field, `Enter` approves the gate a session waits
  at; without a gate that can be approved it approves an orchestrator's proposed split. On the
  Spec tab `Enter` approves only the gate, never a proposed split.
