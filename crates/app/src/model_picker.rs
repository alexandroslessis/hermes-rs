//! A searchable list of the provider's models, used in Settings and in the composer's model
//! popover (the role Zed's agent model selector plays).

use std::rc::Rc;

use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, SharedString,
    Subscription, UniformListScrollHandle, Window, prelude::*, uniform_list,
};
use hermes_llm::discovery::DiscoveredModel;
use ui::{Tooltip, prelude::*};

use crate::state::AppState;
use crate::text_area::{TextArea, TextAreaEvent};

pub type OnSelect = Rc<dyn Fn(String, &mut Window, &mut App)>;

pub struct ModelPicker {
    search: Entity<TextArea>,
    free_only: bool,
    tools_only: bool,
    selected: Option<String>,
    list_height: Pixels,
    in_popover: bool,
    scroll: UniformListScrollHandle,
    on_select: OnSelect,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for ModelPicker {}

fn price(value: Option<f64>) -> String {
    match value {
        Some(0.0) => "free".into(),
        Some(price) => format!("${price:.2}"),
        None => "?".into(),
    }
}

fn context(value: Option<usize>) -> String {
    match value {
        Some(tokens) if tokens >= 1_000_000 => format!("{}M", tokens / 1_000_000),
        Some(tokens) => format!("{}K", tokens / 1000),
        None => "?".into(),
    }
}

impl ModelPicker {
    pub fn new(
        selected: Option<String>,
        list_height: Pixels,
        on_select: OnSelect,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| TextArea::new("Search models…", cx).single_line());
        let state = AppState::global(cx);
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &search,
                window,
                |this: &mut Self, _, event, window, cx| match event {
                    TextAreaEvent::Changed => cx.notify(),
                    // Enter picks the first match.
                    TextAreaEvent::Submit => {
                        if let Some(model) = this.filtered(cx).into_iter().next() {
                            this.select(model.id, window, cx);
                        }
                    }
                },
            ),
        ];
        Self {
            search,
            free_only: false,
            tools_only: true,
            selected,
            list_height,
            in_popover: false,
            scroll: UniformListScrollHandle::new(),
            on_select,
            _subscriptions: subscriptions,
        }
    }

    /// Renders as a floating card (the composer's model popover).
    pub fn in_popover(mut self) -> Self {
        self.in_popover = true;
        self
    }

    pub fn set_selected(&mut self, selected: Option<String>, cx: &mut Context<Self>) {
        self.selected = selected;
        cx.notify();
    }

    fn filtered(&self, cx: &App) -> Vec<DiscoveredModel> {
        let query = self.search.read(cx).text().to_lowercase();
        AppState::global(cx)
            .read(cx)
            .models
            .iter()
            .filter(|model| {
                !self.free_only
                    || model.id.ends_with(":free")
                    || (model.input_price == Some(0.) && model.output_price == Some(0.))
            })
            .filter(|model| !self.tools_only || model.supports_tools != Some(false))
            .filter(|model| {
                query.is_empty()
                    || model.id.to_lowercase().contains(&query)
                    || model
                        .name
                        .as_deref()
                        .is_some_and(|name| name.to_lowercase().contains(&query))
            })
            .cloned()
            .collect()
    }

    fn select(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(id.clone());
        (self.on_select)(id, window, cx);
        cx.emit(DismissEvent);
        cx.notify();
    }
}

impl Focusable for ModelPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }
}

impl Render for ModelPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (loading, total) = {
            let state = AppState::global(cx).read(cx);
            (state.models_loading, state.models.len())
        };
        let models = self.filtered(cx);
        let count = models.len();
        let selected = self.selected.clone();
        v_flex()
            .gap_1()
            .when(self.in_popover, |this| {
                this.w(px(560.)).p_2().elevation_3(cx)
            })
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(colors.border)
                            .bg(colors.editor_background)
                            .child(self.search.clone()),
                    )
                    .child(
                        Button::new("free-only", "Free")
                            .label_size(LabelSize::Small)
                            .toggle_state(self.free_only)
                            .tooltip(Tooltip::text("Only free models"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.free_only = !this.free_only;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("tools-only", "Tools")
                            .label_size(LabelSize::Small)
                            .toggle_state(self.tools_only)
                            .tooltip(Tooltip::text("Only models that support tool calling"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.tools_only = !this.tools_only;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                Label::new(if loading {
                    "Loading models…".to_string()
                } else {
                    format!("{count} of {total} models · USD per million tokens in / out")
                })
                .size(LabelSize::XSmall)
                .color(Color::Muted),
            )
            .child(
                div()
                    .flex_none()
                    .h(self.list_height)
                    .rounded_md()
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.editor_background)
                    .child(
                        uniform_list(
                            "models",
                            count,
                            cx.processor(
                                move |_this, range: std::ops::Range<usize>, _window, cx| {
                                    let colors = cx.theme().colors();
                                    range
                                        .map(|index| {
                                            let model = models[index].clone();
                                            let is_selected =
                                                selected.as_deref() == Some(model.id.as_str());
                                            let id = model.id.clone();
                                            h_flex()
                                                .id(SharedString::from(format!("model-{index}")))
                                                .w_full()
                                                .px_2()
                                                .h(px(28.))
                                                .gap_2()
                                                .cursor_pointer()
                                                .when(is_selected, |row| {
                                                    row.bg(colors.element_selected)
                                                })
                                                .hover(|row| row.bg(colors.ghost_element_hover))
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .overflow_hidden()
                                                        .whitespace_nowrap()
                                                        .text_ellipsis()
                                                        .text_sm()
                                                        .child(model.id.clone()),
                                                )
                                                .when(
                                                    model.supports_reasoning == Some(true),
                                                    |row| {
                                                        row.child(
                                                            Icon::new(IconName::ToolThink)
                                                                .size(IconSize::XSmall)
                                                                .color(Color::Muted),
                                                        )
                                                    },
                                                )
                                                .child(
                                                    Label::new(context(model.context_length))
                                                        .size(LabelSize::XSmall)
                                                        .color(Color::Muted),
                                                )
                                                .child(
                                                    Label::new(format!(
                                                        "{} / {}",
                                                        price(model.input_price),
                                                        price(model.output_price)
                                                    ))
                                                    .size(LabelSize::XSmall)
                                                    .color(Color::Muted),
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.select(id.clone(), window, cx)
                                                    },
                                                ))
                                        })
                                        .collect()
                                },
                            ),
                        )
                        .track_scroll(&self.scroll)
                        .size_full(),
                    ),
            )
    }
}
