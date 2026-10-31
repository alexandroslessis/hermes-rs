# Third-party notices

hermes-rs as a whole is licensed under the GNU General Public License v3.0 or later (`LICENSE`).
It contains code and text copied and adapted from the projects below, which remain under their
original licenses. Exact commits and the list of copied crates are in `vendor/SOURCES.md`.

| Source | What was copied | License | License text |
|---|---|---|---|
| [Zed](https://github.com/zed-industries/zed), © Zed Industries, Inc. | `gpui` and its platform/support crates | Apache-2.0 | `licenses/Apache-2.0.txt` |
| Zed | `ui`, `theme`, `menu`, `component`, `icons`, `util` and other UI crates; fonts, icons and themes from `assets/` | GPL-3.0-or-later | `LICENSE` |
| [async-task (Zed fork)](https://github.com/zed-industries/async-task), © Stjepan Glavina and contributors | `vendor/async-task` | Apache-2.0 OR MIT | `vendor/async-task/LICENSE-*` |
| [goose](https://github.com/aaif-goose/goose), © Block, Inc. and AAIF contributors | Agent state machine, provider types, OpenAI-compatible provider, context management (`crates/agent`, `crates/llm`) | Apache-2.0 | `licenses/Apache-2.0.txt` |
| [Hermes Agent](https://github.com/NousResearch/hermes-agent), © 2025 Nous Research | Prompt text (`prompts/`), bundled skills (`skills/`), tool descriptions, approval patterns | MIT | `licenses/MIT-hermes-agent.txt` |

Fonts bundled from Zed's `assets/fonts` keep their own licenses (SIL Open Font License); see the
license files next to each font.

Files copied from these projects keep any copyright headers they had. Modified files are noted in
the commit history.
