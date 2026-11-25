# t3code reference screenshots

Supplied by the user as the visual ground truth for the native UI (see Phase 2 in
[../../PHASES.md](../../PHASES.md)). **Dark theme only so far.** Window size and scale were not
recorded: measure from the image (pixel size) and don't assume 100% scale.

## The set is not complete, and that is fine

The user took what was quick, not an exhaustive list. **If a screenshot you need is missing, take it
yourself:** t3code is at `C:\Users\alexandros\repos\t3code` (MIT; `apps/web` is the UI). The user has t3code installed and
allows you to use it (their instance and data included, user 2026-11-18; see its README). Change
nothing in it that they would notice afterwards (settings, threads, projects): look, toggle the
theme and put it back. Put it in the state you need and capture the one element or area you need
(cropped, not the whole window unless the frame itself is the subject). Save it here with the naming
below and add a row to the table. Light theme is only worth taking when a checkpoint needs it
(t3code has a light theme: toggle it in its settings). Stop and close only the processes you started.

## Naming

`<screen-or-component>-<detail>[-<light|dark>][-<state>].png`. A file with no theme suffix is dark.

## Files

The "Shows" column is inferred from the file names; the first reader who looks at an image corrects
its row if it is wrong.

| File | Shows |
|---|---|
| `app-empty-dark.png` | Whole window, empty thread (frame, sidebar, empty main area) |
| `app-thread-dark.png` | Whole window with a thread open |
| `app-thread-options.png` | Thread menu / options |
| `app-terminal-open.png` | Terminal drawer open |
| `app-sidebar_closeup-dark.png` | Sidebar close-up |
| `app-sidebar_collapsed.png` | Sidebar collapsed |
| `app-codeblock_closeup-dark.png` | Corrected by CP4.1: "Worked for 1.9s", two Python code blocks (language icon left, wrap and copy right), inline code between them |
| `app-hover_reply_closeup-dark.png` | Corrected by CP4.1: the hover footer under a reply: copy, branch and reply icons, then the time (`6:25 PM`) |
| `sidebar.png` | Sidebar |
| `sidebar-right.png` | Right sidebar / panel |
| `sidebar-diff.png`, `sidebar-diff-content.png` | Diff panel and its content |
| `sidebar-filetree.png` | File tree panel |
| `sidebar-thread-details.png`, `sidebar-thread-details-hover.png` | Corrected by CP2.2: the thread details card (Open in Zed, branch, Commit, Changes, Context), a glass panel |
| `button-session-variants-default.png` | Corrected by CP2.2: the three ghost icon buttons of the sidebar's search row (filter, add folder, new thread) |
| `button-session-variants-hover.png` | Not looked at |
| `button-session-variants-selected.png` | Corrected by CP2.2: three thread rows (project monogram and name, time; bold title; branch), the middle one selected |
| `button-sidebar_toggle-hover.png` | Corrected by CP2.2: the topbar's three panel toggles (the middle one hovered, with its "Toggle terminal drawer (Ctrl+J)" tooltip) and the Windows caption buttons |
| `buttons-sidebar-down.png` | Sidebar footer: settings, source control and usage icons, refresh at the right |
| `composer-model-picker.png`, `composer-model-picker-permission.png` | Composer model picker, and with the permission selector |

CP2.2 took no new screenshot: t3code was not running and nothing available to the implementer can
drive its window (no input automation), so the light theme, tooltip, toast, badge, kbd, palette
and empty-state were built from `index.css` and the source named in each module header; the visual
check should say what differs.

CP4.1 took no new screenshot either (same reason: no input automation for t3code's window was
available to the implementer): the light thread, alerts, tables, a user message with attachments and the
reasoning row follow `ChatMarkdown.tsx`, `TimelineUserMessageRow.tsx` and `TimelineWorkEntryRow.tsx`.

CP4.2 took no new screenshot either (t3code's window cannot be driven from here): the command card, file edit
card and diff, the work-group row, the changed-files card and the light theme follow `WorkLog.tsx`,
`TimelineWorkGroupRows.tsx`, `CommandOutputPanel.tsx`, `ChangedFilesTree.tsx`, `DiffStatLabel.tsx` and
`index.css` (`--diff-*`); `app-thread-dark.png` and `sidebar-diff-content.png` were the colour references.

## Not covered yet (take them when the checkpoint needs them)

Light theme; the button variants and sizes in the design system's sense (default, outline, ghost,
destructive); title bar and Windows caption buttons; command palette; toast; tooltip; badge and kbd;
empty states. Phase 2 (CP2.1 tokens, CP2.2 components and frame) needs most of these.

CP4.3 took no new screenshot (t3code's window cannot be driven from here): the header follows `ChatHeader.tsx`
(breadcrumb, rename field, title menu) checked against `app-thread-dark.png` and `app-thread-options.png`; the
branch and change counts follow `sidebar-thread-details.png`; the light theme follows `index.css`.

CP5.1 took no new screenshot (t3code's window cannot be driven from here): the composer follows `ChatComposer.tsx`,
`ComposerSurface.tsx`, `ComposerPrimaryActions.tsx`, `ComposerFooterControls.tsx`, `QueuedRunsControl.tsx` and the existing
`app-thread-dark.png`, `app-empty-dark.png`, `composer-model-picker.png`, `composer-model-picker-permission.png`; the light
theme, the grown input, the queue strip, the mode menu in isolation and the context meter follow the source.

CP5.2 took no new screenshot either (the window cannot be driven from here, and nothing was changed in the user's t3code): the
`@`/`/` drawer follows `ComposerCommandMenu.tsx` and `ComposerCommandMenuLayer.tsx`, the strip `ComposerAttachmentStrip.tsx`, the
chip `ContextChip.tsx`; the sent thumbnails reuse `app-hover_reply_closeup-dark.png`. No README row to add: no file was added.

CP5.3: no capture of the approval or question panel exists here (t3code has none of a pending approval: it shows it in the timeline row, and the question as the composer top drawer; I did not drive the user's t3code window). The panel is built from `ComposerBanner.tsx`, `ComposerPendingApprovalActions.tsx`, `TimelinePendingApprovalRow.tsx` and `ComposerPendingUserInputPanel.tsx` (source only), on the card of `app-thread-dark.png`.
