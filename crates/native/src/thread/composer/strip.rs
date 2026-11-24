//! What the composer draws for mentions and attachments around the input: the strip of attached
//! files above the text (images as 64 px tiles, other files as rows with a name and a size, each
//! with a remove button; the strip wraps and, with many files, scrolls) and the chip a mention is
//! drawn as inside the text.
//!
//! t3code: `components/chat/ComposerAttachmentStrip.tsx` (`h-16 w-16` tiles, `rounded-lg`,
//! `border-border/80`, the round remove button at the top right, file rows with a name and a
//! muted size), `ComposerImageThumbnail.tsx`, `components/ContextChip.tsx` (a mention is its
//! icon and name in the chip's accent colour, medium weight, inline with the text). Reference
//! screenshots: `app-hover_reply_closeup-dark.png` (the thumbnails of a sent message); the strip
//! and the chip have no capture in `docs/native-ui/reference/t3code/`, see its README.

use gpui_kit::{
    AnyElement, Context, ElementId, FontWeight, ImageSource, InteractiveElement as _, IntoElement,
    ObjectFit, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Window,
    assets::IconName,
    component::{Sizable as _, Size as KitSize, input::InlineTokenContext},
    div, img,
    prelude::FluentBuilder as _,
    px,
};

use super::attach::{Attachment, Kind, format_size};
use super::mention::Chip;
use super::suggest::chip_icon;
use super::view::ComposerView;
use crate::design::{
    Button,
    button::{Size, Variant},
    icons::icon,
    tokens::{Palette, radius, text},
};
use crate::dev::ProbeExt as _;
use crate::theme::DesignTheme as _;

/// An image tile's side (`h-16 w-16`).
pub const TILE: f32 = 64.;
/// The strip's tallest before it scrolls: two rows of tiles (136 px), the gap, and 22 px of what
/// follows, so the cut shows a third of a tile or most of a file row (at 152 px only the top
/// 2 px of the next row's icon showed: a stray dash, not a hint that the strip scrolls).
pub const MAX_HEIGHT: f32 = 166.;

impl ComposerView {
    /// The strip above the input (`None` with nothing attached or being read).
    pub(super) fn attachment_strip(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let model = self.model.read(cx);
        let attachments: Vec<Attachment> = model.attachments().to_vec();
        let preparing = model.preparing();
        if attachments.is_empty() && preparing == 0 {
            return None;
        }
        let tiles: Vec<AnyElement> = attachments
            .iter()
            .filter(|a| a.kind == Kind::Image)
            .map(|a| self.image_tile(a, palette, cx))
            .chain((0..preparing).map(|ix| preparing_tile(ix, palette)))
            .collect();
        let rows: Vec<AnyElement> = attachments
            .iter()
            .filter(|a| a.kind == Kind::File)
            .map(|a| self.file_row(a, palette))
            .collect();
        Some(
            div()
                .id("composer-attachments")
                .flex()
                .flex_col()
                .gap(px(8.))
                .mb(px(12.))
                .max_h(px(MAX_HEIGHT))
                .overflow_y_scroll()
                .when(!tiles.is_empty(), |d| {
                    d.child(
                        div()
                            .id("attachment-tiles")
                            .flex()
                            .flex_wrap()
                            .gap(px(8.))
                            .children(tiles)
                            .probe(),
                    )
                })
                .when(!rows.is_empty(), |d| {
                    d.child(
                        div()
                            .id("attachment-files")
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .children(rows)
                            .probe(),
                    )
                })
                .probe()
                .into_any_element(),
        )
    }

    fn image_tile(
        &self,
        attachment: &Attachment,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = attachment.id;
        let thumb = self.model.read(cx).thumb(id);
        let tip: SharedString =
            format!("{} ({})", attachment.name, format_size(attachment.size)).into();
        div()
            .id(("attach-tile", id as usize))
            .relative()
            .flex_shrink_0()
            .w(px(TILE))
            .h(px(TILE))
            .rounded(radius::LG)
            .overflow_hidden()
            .border_1()
            .border_color(palette.border.with_alpha(204).hsla())
            .bg(palette.background.hsla())
            .tooltip(move |window, cx| crate::design::tooltip::view(tip.clone(), None, window, cx))
            .child(match thumb {
                // The image is rounded itself: the tile's `overflow_hidden` clips to its box, not
                // to its rounded corners, so the picture's corners showed past the border.
                Some(image) => img(ImageSource::Image(image))
                    .size_full()
                    .rounded(radius::LG)
                    .object_fit(ObjectFit::Cover)
                    .into_any_element(),
                // An image too large to decode here: still attached, drawn as an icon.
                None => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        icon(IconName::Image)
                            .with_size(KitSize::Size(px(16.)))
                            .text_color(palette.secondary_label.hsla()),
                    )
                    .into_any_element(),
            })
            .child(self.remove_button(attachment, true, palette))
            .probe()
            .into_any_element()
    }

    fn file_row(
        &self,
        attachment: &Attachment,
        palette: &Palette,
    ) -> AnyElement {
        let id = attachment.id;
        let name: SharedString = attachment.name.clone().into();
        div()
            .id(("attach-file", id as usize))
            .flex()
            .items_center()
            .gap(px(8.))
            .min_w_0()
            .h(px(28.))
            .text_size(text::SM.size)
            .text_color(palette.foreground.hsla())
            .child(
                icon(IconName::FileText)
                    .with_size(KitSize::Size(px(14.)))
                    .text_color(palette.muted_foreground.hsla()),
            )
            .child(
                div()
                    .id(("attach-name", id as usize))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(name.clone())
                    .probe_text(name),
            )
            .child(
                div()
                    .id(("attach-size", id as usize))
                    .flex_shrink_0()
                    .text_size(text::XS.size)
                    .text_color(palette.secondary_label.hsla())
                    .child(format_size(attachment.size))
                    .probe(),
            )
            .child(self.remove_button(attachment, false, palette))
            .probe()
            .into_any_element()
    }

    /// The remove button: round and over the corner of a tile, a quiet icon button on a row.
    fn remove_button(
        &self,
        attachment: &Attachment,
        on_tile: bool,
        palette: &Palette,
    ) -> AnyElement {
        let (model, id) = (self.model.clone(), attachment.id);
        let label: SharedString = format!("Remove {}", attachment.name).into();
        let button = Button::icon_only(("attach-remove", id as usize), IconName::Close)
            .tooltip(label)
            .variant(Variant::GhostMuted)
            .size(Size::Xs)
            .on_click(move |_, _, cx| model.update(cx, |m, cx| m.remove_attachment(id, cx)));
        if on_tile {
            // t3code's `media-close`: a dark disc with a white cross, whatever the theme.
            div()
                .absolute()
                .top(px(2.))
                .right(px(2.))
                .rounded_full()
                .bg(palette.background.with_alpha(204).hsla())
                .child(button)
                .into_any_element()
        } else {
            button.into_any_element()
        }
    }
}

/// A tile for a file being read.
fn preparing_tile(ix: usize, palette: &Palette) -> AnyElement {
    div()
        .id(("attach-preparing", ix))
        .flex_shrink_0()
        .w(px(TILE))
        .h(px(TILE))
        .flex()
        .items_center()
        .justify_center()
        .rounded(radius::LG)
        .border_1()
        .border_color(palette.border.with_alpha(204).hsla())
        .bg(palette.muted.with_alpha(128).hsla())
        .text_size(text::XXS2.size)
        .text_color(palette.secondary_label.hsla())
        .child("Preparing")
        .probe()
        .into_any_element()
}

/// A mention inside the text: its icon and name in a blue ink with no fill, as t3code's context
/// chips (`ContextChip.tsx`: `text-(--context-chip-ink)`, medium weight, no background). t3code's
/// file accent (`oklch(0.62 0.136 237)`, `lib/contextChipAccents.ts`) is solved per theme to a
/// 5:1 contrast on the surfaces (`themePalette.ts`, `contextChipVariables`); the palette's
/// `info_foreground` is the theme's blue text at that contrast (#51a2ff dark, #1447e6 light), so
/// it stands in for it. A selected chip is filled lightly, like selected text.
pub fn chip_element(
    context: &InlineTokenContext,
    _window: &mut Window,
    cx: &mut gpui_kit::App,
) -> AnyElement {
    let palette = cx.design_theme().palette;
    let kind = Chip::kind_of_id(context.token().id());
    let ink = palette.info_foreground.hsla();
    div()
        .id(("mention-chip", context.range().start))
        .flex()
        .items_center()
        .gap(px(3.))
        .px(px(1.))
        .h(context.line_height())
        .max_w(context.available_width())
        .rounded(radius::SM)
        .when(context.is_selected(), |d| d.bg(ink.opacity(0.24)))
        .text_color(ink)
        .font_weight(FontWeight::MEDIUM)
        .child(
            div()
                .flex_shrink_0()
                .child(icon(chip_icon(kind)).with_size(KitSize::Size(px(13.)))),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .child(context.token().label().clone()),
        )
        .probe()
        .into_any_element()
}

/// The element id of an attachment's remove button, for tests.
pub fn remove_id(id: u64) -> ElementId {
    ("attach-remove", id as usize).into()
}
