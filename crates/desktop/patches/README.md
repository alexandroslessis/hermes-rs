# Frontend patches

`scripts/build-hermes-webapp.ps1` applies every `*.patch` here (in name order) to Hermes's
exported source before building the webapp. A patch that no longer applies stops the build:
refresh it against the new Hermes commit (edit the exported file, then diff it against
`git -C ../hermes-agent show <commit>:<path>`).

- **0001-queued-prompt-bubble.patch** (`use-message-stream/gateway-event/message-stream.ts`):
  a queued prompt the backend starts had no user bubble (none until the session was reloaded):
  with the canonical protocol the backend owns the queue and no event draws one. The backend's
  `message.start` names the started admission as `turn_id` (sent before the `session.info` that
  retires its card); the patch turns that queue card into the user's bubble, id
  `user-<admission>` so the turn's `persisted_turn` binds its stored row. Redirects and missed
  steers re-run from the queue are drawn by the composer already, so the backend doesn't list
  them as cards (`shown`). Tested live 2026-11-04: a queued prompt shows its bubble when its
  turn starts, once (also after a reload); Enter-steer shows once.

## To do

- **Steering a queued card shows the message only after the reply.** `prompt.steer_queued`
  hands the text to the running turn and retires the card; nothing draws a bubble (Hermes
  upstream too). A first attempt (5ed51ba, reverted) made the card steer the composer's way
  (`steerQueuedNow`, then `prompt.cancel`) and left the Steer button dead in the app. Next try:
  draw the bubble from `prompt.steer_queued`'s success in `queue-slot.tsx`, and test it live.
