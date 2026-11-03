// hermes-rs: Ctrl+Enter steers the running turn (Enter queues, as display.busy_input_mode "queue"
// makes it). Hermes's composer fixes Ctrl+Enter to queue, so this catches it first and presses
// that composer's Steer button (its `debug-step-over` icon), which shows only while a turn runs
// and the draft can steer. Otherwise Hermes's own handling goes ahead.
document.addEventListener(
  'keydown',
  function (event) {
    if (event.key !== 'Enter' || !(event.ctrlKey || event.metaKey) || event.shiftKey || event.altKey) {
      return
    }
    var composer = event.target && event.target.closest && event.target.closest('form')
    var icon = composer && composer.querySelector('button .codicon-debug-step-over')
    var steer = icon && icon.closest('button')
    if (steer && !steer.disabled) {
      event.preventDefault()
      event.stopImmediatePropagation()
      steer.click()
    }
  },
  true
)
