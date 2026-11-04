# Frontend patches

`scripts/build-hermes-webapp.ps1` applies every `*.patch` here (in name order) to Hermes's
exported source before building the webapp. A patch that no longer applies stops the build.

## To do

- **Steering a queued message doesn't show it until the reply ends.** The queue panel's Steer
  on a server-queued row calls `prompt.steer_queued`, and nothing draws the user's bubble
  (Hermes upstream behaves the same). A first attempt (reverted, 5ed51ba) made the row steer
  the composer's way (`steerQueuedNow`, then `prompt.cancel`) in
  `apps/desktop/src/app/chat/composer/queue-slot.tsx`; in the app the Steer button then did
  nothing, and queued prompts lost their bubble before the next reply. Find why
  `steerQueuedNow` refuses server rows (likely its `isSteerableEntry` / `onSteer` / `busy`
  guards) before trying again, or draw the bubble from the `prompt.steer_queued` result instead.
- **A queued message's bubble appears only after its reply.** Same root cause, for every
  queued message (not just steered ones): with the canonical protocol (`native_dial` on the
  socket URL) the backend owns the queue (`serverOwnsComposerQueue`), and when it starts a
  queued admission no event draws the user's bubble; it shows when the transcript reloads.
  `native_dial` also selects the whole canonical contract (receipts, approvals, mutations), so
  it can't be dropped to hand the queue back to the page. Fix in the frontend: draw the bubble
  when a pending row the page knows turns `started` (`store/pending-submissions.ts`,
  `projectQueue`), which covers both cases.
