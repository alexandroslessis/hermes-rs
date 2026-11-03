// hermes-rs's built-in desktop plugin: hides the parts of Hermes's interface whose backend
// hermes-rs doesn't implement, through Hermes's own plugin API (the frontend stays unmodified).
// The backend writes this file to <data folder>/desktop-plugins/hermes-rs/plugin.js on start.
//
// Hidden sidebar rows (Hermes's `sidebarNav.prefs` area; Capabilities can't be hidden — it
// hosts the Plugins tab):
//   messaging  — Telegram/Discord/Slack bridges (Hermes's gateway platforms)
//   artifacts  — the artifacts index
//   cron       — scheduled jobs

export default {
  id: 'hermes-rs',
  name: 'hermes-rs',
  description: 'Hides features the hermes-rs backend does not implement yet.',
  register(ctx) {
    ctx.register({
      id: 'hide-unsupported',
      area: 'sidebarNav.prefs',
      data: { hide: ['messaging', 'artifacts', 'cron'] }
    })
  }
}
