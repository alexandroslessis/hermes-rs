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
