//! Theme file content types, copied from Zed's `settings_content::theme`.
#![allow(missing_docs)]

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::{fmt::Display, ops::Deref};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThemeColor(String);

impl From<String> for ThemeColor {
    fn from(value: String) -> Self {
        ThemeColor(value)
    }
}

impl From<&str> for ThemeColor {
    fn from(value: &str) -> Self {
        ThemeColor(value.to_string())
    }
}

impl Deref for ThemeColor {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<ThemeColor> for String {
    fn from(value: ThemeColor) -> Self {
        value.0
    }
}

impl Display for ThemeColor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<&ThemeColor> for gpui::Rgba {
    type Error = anyhow::Error;

    fn try_from(value: &ThemeColor) -> anyhow::Result<Self> {
        let s: &str = &value.0;
        gpui::Rgba::try_from(s)
    }
}

/// The content of a serialized theme.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ThemeStyleContent {
    #[serde(rename = "background.appearance")]
    pub window_background_appearance: Option<WindowBackgroundContent>,

    #[serde(default)]
    pub accents: Vec<AccentContent>,

    #[serde(flatten, default)]
    pub colors: ThemeColorsContent,

    #[serde(flatten, default)]
    pub status: StatusColorsContent,

    #[serde(default)]
    pub players: Vec<PlayerColorContent>,

    /// The styles for syntax nodes.
    #[serde(default)]
    pub syntax: IndexMap<String, HighlightStyleContent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccentContent(pub Option<ThemeColor>);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlayerColorContent {
    pub cursor: Option<ThemeColor>,
    pub background: Option<ThemeColor>,
    pub selection: Option<ThemeColor>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ThemeColorsContent {
    /// Border color. Used for most borders, is usually a high contrast color.
    #[serde(rename = "border")]
    pub border: Option<ThemeColor>,

    /// Border color. Used for deemphasized borders, like a visual divider between two sections
    #[serde(rename = "border.variant")]
    pub border_variant: Option<ThemeColor>,

    /// Border color. Used for focused elements, like keyboard focused list item.
    #[serde(rename = "border.focused")]
    pub border_focused: Option<ThemeColor>,

    /// Border color. Used for selected elements, like an active search filter or selected checkbox.
    #[serde(rename = "border.selected")]
    pub border_selected: Option<ThemeColor>,

    /// Border color. Used for transparent borders. Used for placeholder borders when an element gains a border on state change.
    #[serde(rename = "border.transparent")]
    pub border_transparent: Option<ThemeColor>,

    /// Border color. Used for disabled elements, like a disabled input or button.
    #[serde(rename = "border.disabled")]
    pub border_disabled: Option<ThemeColor>,

    /// Background color. Used for elevated surfaces, like a context menu, popup, or dialog.
    #[serde(rename = "elevated_surface.background")]
    pub elevated_surface_background: Option<ThemeColor>,

    /// Background Color. Used for grounded surfaces like a panel or tab.
    #[serde(rename = "surface.background")]
    pub surface_background: Option<ThemeColor>,

    /// Background Color. Used for the app background and blank panels or windows.
    #[serde(rename = "background")]
    pub background: Option<ThemeColor>,

    /// Background Color. Used for the background of an element that should have a different background than the surface it's on.
    ///
    /// Elements might include: Buttons, Inputs, Checkboxes, Radio Buttons...
    ///
    /// For an element that should have the same background as the surface it's on, use `ghost_element_background`.
    #[serde(rename = "element.background")]
    pub element_background: Option<ThemeColor>,

    /// Background Color. Used for the hover state of an element that should have a different background than the surface it's on.
    ///
    /// Hover states are triggered by the mouse entering an element, or a finger touching an element on a touch screen.
    #[serde(rename = "element.hover")]
    pub element_hover: Option<ThemeColor>,

    /// Background Color. Used for the active state of an element that should have a different background than the surface it's on.
    ///
    /// Active states are triggered by the mouse button being pressed down on an element, or the Return button or other activator being pressed.
    #[serde(rename = "element.active")]
    pub element_active: Option<ThemeColor>,

    /// Background Color. Used for the selected state of an element that should have a different background than the surface it's on.
    ///
    /// Selected states are triggered by the element being selected (or "activated") by the user.
    ///
    /// This could include a selected checkbox, a toggleable button that is toggled on, etc.
    #[serde(rename = "element.selected")]
    pub element_selected: Option<ThemeColor>,

    /// Background Color. Used for the disabled state of an element that should have a different background than the surface it's on.
    ///
    /// Disabled states are shown when a user cannot interact with an element, like a disabled button or input.
    #[serde(rename = "element.disabled")]
    pub element_disabled: Option<ThemeColor>,

    /// Background Color. Used for the background of selections in a UI element.
    #[serde(rename = "element.selection_background")]
    pub element_selection_background: Option<ThemeColor>,

    /// Background Color. Used for the area that shows where a dragged element will be dropped.
    #[serde(rename = "drop_target.background")]
    pub drop_target_background: Option<ThemeColor>,

    /// Border Color. Used for the border that shows where a dragged element will be dropped.
    #[serde(rename = "drop_target.border")]
    pub drop_target_border: Option<ThemeColor>,

    /// Used for the background of a ghost element that should have the same background as the surface it's on.
    ///
    /// Elements might include: Buttons, Inputs, Checkboxes, Radio Buttons...
    ///
    /// For an element that should have a different background than the surface it's on, use `element_background`.
    #[serde(rename = "ghost_element.background")]
    pub ghost_element_background: Option<ThemeColor>,

    /// Background Color. Used for the hover state of a ghost element that should have the same background as the surface it's on.
    ///
    /// Hover states are triggered by the mouse entering an element, or a finger touching an element on a touch screen.
    #[serde(rename = "ghost_element.hover")]
    pub ghost_element_hover: Option<ThemeColor>,

    /// Background Color. Used for the active state of a ghost element that should have the same background as the surface it's on.
    ///
    /// Active states are triggered by the mouse button being pressed down on an element, or the Return button or other activator being pressed.
    #[serde(rename = "ghost_element.active")]
    pub ghost_element_active: Option<ThemeColor>,

    /// Background Color. Used for the selected state of a ghost element that should have the same background as the surface it's on.
    ///
    /// Selected states are triggered by the element being selected (or "activated") by the user.
    ///
    /// This could include a selected checkbox, a toggleable button that is toggled on, etc.
    #[serde(rename = "ghost_element.selected")]
    pub ghost_element_selected: Option<ThemeColor>,

    /// Background Color. Used for the disabled state of a ghost element that should have the same background as the surface it's on.
    ///
    /// Disabled states are shown when a user cannot interact with an element, like a disabled button or input.
    #[serde(rename = "ghost_element.disabled")]
    pub ghost_element_disabled: Option<ThemeColor>,

    /// Text Color. Default text color used for most text.
    #[serde(rename = "text")]
    pub text: Option<ThemeColor>,

    /// Text Color. Color of muted or deemphasized text. It is a subdued version of the standard text color.
    #[serde(rename = "text.muted")]
    pub text_muted: Option<ThemeColor>,

    /// Text Color. Color of the placeholder text typically shown in input fields to guide the user to enter valid data.
    #[serde(rename = "text.placeholder")]
    pub text_placeholder: Option<ThemeColor>,

    /// Text Color. Color used for text denoting disabled elements. Typically, the color is faded or grayed out to emphasize the disabled state.
    #[serde(rename = "text.disabled")]
    pub text_disabled: Option<ThemeColor>,

    /// Text Color. Color used for emphasis or highlighting certain text, like an active filter or a matched character in a search.
    #[serde(rename = "text.accent")]
    pub text_accent: Option<ThemeColor>,

    /// Fill Color. Used for the default fill color of an icon.
    #[serde(rename = "icon")]
    pub icon: Option<ThemeColor>,

    /// Fill Color. Used for the muted or deemphasized fill color of an icon.
    ///
    /// This might be used to show an icon in an inactive pane, or to deemphasize a series of icons to give them less visual weight.
    #[serde(rename = "icon.muted")]
    pub icon_muted: Option<ThemeColor>,

    /// Fill Color. Used for the disabled fill color of an icon.
    ///
    /// Disabled states are shown when a user cannot interact with an element, like a icon button.
    #[serde(rename = "icon.disabled")]
    pub icon_disabled: Option<ThemeColor>,

    /// Fill Color. Used for the placeholder fill color of an icon.
    ///
    /// This might be used to show an icon in an input that disappears when the user enters text.
    #[serde(rename = "icon.placeholder")]
    pub icon_placeholder: Option<ThemeColor>,

    /// Fill Color. Used for the accent fill color of an icon.
    ///
    /// This might be used to show when a toggleable icon button is selected.
    #[serde(rename = "icon.accent")]
    pub icon_accent: Option<ThemeColor>,

    /// Color used to accent some of the debuggers elements
    /// Only accent breakpoint & breakpoint related symbols right now
    #[serde(rename = "debugger.accent")]
    pub debugger_accent: Option<ThemeColor>,

    #[serde(rename = "status_bar.background")]
    pub status_bar_background: Option<ThemeColor>,

    #[serde(rename = "title_bar.background")]
    pub title_bar_background: Option<ThemeColor>,

    #[serde(rename = "title_bar.inactive_background")]
    pub title_bar_inactive_background: Option<ThemeColor>,

    #[serde(rename = "toolbar.background")]
    pub toolbar_background: Option<ThemeColor>,

    #[serde(rename = "tab_bar.background")]
    pub tab_bar_background: Option<ThemeColor>,

    #[serde(rename = "tab.inactive_background")]
    pub tab_inactive_background: Option<ThemeColor>,

    #[serde(rename = "tab.active_background")]
    pub tab_active_background: Option<ThemeColor>,

    #[serde(rename = "search.match_background")]
    pub search_match_background: Option<ThemeColor>,

    #[serde(rename = "search.active_match_background")]
    pub search_active_match_background: Option<ThemeColor>,

    #[serde(rename = "panel.background")]
    pub panel_background: Option<ThemeColor>,

    #[serde(rename = "panel.focused_border")]
    pub panel_focused_border: Option<ThemeColor>,

    #[serde(rename = "panel.indent_guide")]
    pub panel_indent_guide: Option<ThemeColor>,

    #[serde(rename = "panel.indent_guide_hover")]
    pub panel_indent_guide_hover: Option<ThemeColor>,

    #[serde(rename = "panel.indent_guide_active")]
    pub panel_indent_guide_active: Option<ThemeColor>,

    #[serde(rename = "panel.overlay_background")]
    pub panel_overlay_background: Option<ThemeColor>,

    #[serde(rename = "panel.overlay_hover")]
    pub panel_overlay_hover: Option<ThemeColor>,

    #[serde(rename = "pane.focused_border")]
    pub pane_focused_border: Option<ThemeColor>,

    #[serde(rename = "pane_group.border")]
    pub pane_group_border: Option<ThemeColor>,

    /// The deprecated version of `scrollbar.thumb.background`.
    ///
    /// Don't use this field.
    #[serde(rename = "scrollbar_thumb.background", skip_serializing)]
    pub deprecated_scrollbar_thumb_background: Option<ThemeColor>,

    /// The color of the scrollbar thumb.
    #[serde(rename = "scrollbar.thumb.background")]
    pub scrollbar_thumb_background: Option<ThemeColor>,

    /// The color of the scrollbar thumb when hovered over.
    #[serde(rename = "scrollbar.thumb.hover_background")]
    pub scrollbar_thumb_hover_background: Option<ThemeColor>,

    /// The color of the scrollbar thumb whilst being actively dragged.
    #[serde(rename = "scrollbar.thumb.active_background")]
    pub scrollbar_thumb_active_background: Option<ThemeColor>,

    /// The border color of the scrollbar thumb.
    #[serde(rename = "scrollbar.thumb.border")]
    pub scrollbar_thumb_border: Option<ThemeColor>,

    /// The background color of the scrollbar track.
    #[serde(rename = "scrollbar.track.background")]
    pub scrollbar_track_background: Option<ThemeColor>,

    /// The border color of the scrollbar track.
    #[serde(rename = "scrollbar.track.border")]
    pub scrollbar_track_border: Option<ThemeColor>,

    /// The color of the minimap thumb.
    #[serde(rename = "minimap.thumb.background")]
    pub minimap_thumb_background: Option<ThemeColor>,

    /// The color of the minimap thumb when hovered over.
    #[serde(rename = "minimap.thumb.hover_background")]
    pub minimap_thumb_hover_background: Option<ThemeColor>,

    /// The color of the minimap thumb whilst being actively dragged.
    #[serde(rename = "minimap.thumb.active_background")]
    pub minimap_thumb_active_background: Option<ThemeColor>,

    /// The border color of the minimap thumb.
    #[serde(rename = "minimap.thumb.border")]
    pub minimap_thumb_border: Option<ThemeColor>,

    #[serde(rename = "editor.foreground")]
    pub editor_foreground: Option<ThemeColor>,

    /// Text color used for CodeLens items in the editor.
    #[serde(rename = "editor.code_lens.foreground")]
    pub editor_code_lens_foreground: Option<ThemeColor>,

    #[serde(rename = "editor.background")]
    pub editor_background: Option<ThemeColor>,

    #[serde(rename = "editor.gutter.background")]
    pub editor_gutter_background: Option<ThemeColor>,

    #[serde(rename = "editor.subheader.background")]
    pub editor_subheader_background: Option<ThemeColor>,

    #[serde(rename = "editor.active_line.background")]
    pub editor_active_line_background: Option<ThemeColor>,

    #[serde(rename = "editor.highlighted_line.background")]
    pub editor_highlighted_line_background: Option<ThemeColor>,

    /// Background of active line of debugger
    #[serde(rename = "editor.debugger_active_line.background")]
    pub editor_debugger_active_line_background: Option<ThemeColor>,

    /// Text Color. Used for the text of the line number in the editor gutter.
    #[serde(rename = "editor.line_number")]
    pub editor_line_number: Option<ThemeColor>,

    /// Text Color. Used for the text of the line number in the editor gutter when the line is highlighted.
    #[serde(rename = "editor.active_line_number")]
    pub editor_active_line_number: Option<ThemeColor>,

    /// Text Color. Used for the text of the line number in the editor gutter when the line is hovered over.
    #[serde(rename = "editor.hover_line_number")]
    pub editor_hover_line_number: Option<ThemeColor>,

    /// Text Color. Used to mark invisible characters in the editor.
    ///
    /// Example: spaces, tabs, carriage returns, etc.
    #[serde(rename = "editor.invisible")]
    pub editor_invisible: Option<ThemeColor>,

    #[serde(rename = "editor.wrap_guide")]
    pub editor_wrap_guide: Option<ThemeColor>,

    #[serde(rename = "editor.active_wrap_guide")]
    pub editor_active_wrap_guide: Option<ThemeColor>,

    #[serde(rename = "editor.indent_guide")]
    pub editor_indent_guide: Option<ThemeColor>,

    #[serde(rename = "editor.indent_guide_active")]
    pub editor_indent_guide_active: Option<ThemeColor>,

    /// Read-access of a symbol, like reading a variable.
    ///
    /// A document highlight is a range inside a text document which deserves
    /// special attention. Usually a document highlight is visualized by changing
    /// the background color of its range.
    #[serde(rename = "editor.document_highlight.read_background")]
    pub editor_document_highlight_read_background: Option<ThemeColor>,

    /// Read-access of a symbol, like reading a variable.
    ///
    /// A document highlight is a range inside a text document which deserves
    /// special attention. Usually a document highlight is visualized by changing
    /// the background color of its range.
    #[serde(rename = "editor.document_highlight.write_background")]
    pub editor_document_highlight_write_background: Option<ThemeColor>,

    /// Highlighted brackets background color.
    ///
    /// Matching brackets in the cursor scope are highlighted with this background color.
    #[serde(rename = "editor.document_highlight.bracket_background")]
    pub editor_document_highlight_bracket_background: Option<ThemeColor>,

    /// Filled background color for added diff hunk row highlights in the editor.
    #[serde(rename = "editor.diff_hunk.added.background")]
    pub editor_diff_hunk_added_background: Option<ThemeColor>,

    /// Hollow background color for added diff hunk row highlights in the editor.
    #[serde(rename = "editor.diff_hunk.added.hollow_background")]
    pub editor_diff_hunk_added_hollow_background: Option<ThemeColor>,

    /// Hollow border color for added diff hunk row highlights in the editor.
    #[serde(rename = "editor.diff_hunk.added.hollow_border")]
    pub editor_diff_hunk_added_hollow_border: Option<ThemeColor>,

    /// Filled background color for deleted diff hunk row highlights in the editor.
    #[serde(rename = "editor.diff_hunk.deleted.background")]
    pub editor_diff_hunk_deleted_background: Option<ThemeColor>,

    /// Hollow background color for deleted diff hunk row highlights in the editor.
    #[serde(rename = "editor.diff_hunk.deleted.hollow_background")]
    pub editor_diff_hunk_deleted_hollow_background: Option<ThemeColor>,

    /// Hollow border color for deleted diff hunk row highlights in the editor.
    #[serde(rename = "editor.diff_hunk.deleted.hollow_border")]
    pub editor_diff_hunk_deleted_hollow_border: Option<ThemeColor>,

    /// Terminal background color.
    #[serde(rename = "terminal.background")]
    pub terminal_background: Option<ThemeColor>,

    /// Terminal foreground color.
    #[serde(rename = "terminal.foreground")]
    pub terminal_foreground: Option<ThemeColor>,

    /// Terminal ANSI background color.
    #[serde(rename = "terminal.ansi.background")]
    pub terminal_ansi_background: Option<ThemeColor>,

    /// Bright terminal foreground color.
    #[serde(rename = "terminal.bright_foreground")]
    pub terminal_bright_foreground: Option<ThemeColor>,

    /// Dim terminal foreground color.
    #[serde(rename = "terminal.dim_foreground")]
    pub terminal_dim_foreground: Option<ThemeColor>,

    /// Black ANSI terminal color.
    #[serde(rename = "terminal.ansi.black")]
    pub terminal_ansi_black: Option<ThemeColor>,

    /// Bright black ANSI terminal color.
    #[serde(rename = "terminal.ansi.bright_black")]
    pub terminal_ansi_bright_black: Option<ThemeColor>,

    /// Dim black ANSI terminal color.
    #[serde(rename = "terminal.ansi.dim_black")]
    pub terminal_ansi_dim_black: Option<ThemeColor>,

    /// Red ANSI terminal color.
    #[serde(rename = "terminal.ansi.red")]
    pub terminal_ansi_red: Option<ThemeColor>,

    /// Bright red ANSI terminal color.
    #[serde(rename = "terminal.ansi.bright_red")]
    pub terminal_ansi_bright_red: Option<ThemeColor>,

    /// Dim red ANSI terminal color.
    #[serde(rename = "terminal.ansi.dim_red")]
    pub terminal_ansi_dim_red: Option<ThemeColor>,

    /// Green ANSI terminal color.
    #[serde(rename = "terminal.ansi.green")]
    pub terminal_ansi_green: Option<ThemeColor>,

    /// Bright green ANSI terminal color.
    #[serde(rename = "terminal.ansi.bright_green")]
    pub terminal_ansi_bright_green: Option<ThemeColor>,

    /// Dim green ANSI terminal color.
    #[serde(rename = "terminal.ansi.dim_green")]
    pub terminal_ansi_dim_green: Option<ThemeColor>,

    /// Yellow ANSI terminal color.
    #[serde(rename = "terminal.ansi.yellow")]
    pub terminal_ansi_yellow: Option<ThemeColor>,

    /// Bright yellow ANSI terminal color.
    #[serde(rename = "terminal.ansi.bright_yellow")]
    pub terminal_ansi_bright_yellow: Option<ThemeColor>,

    /// Dim yellow ANSI terminal color.
    #[serde(rename = "terminal.ansi.dim_yellow")]
    pub terminal_ansi_dim_yellow: Option<ThemeColor>,

    /// Blue ANSI terminal color.
    #[serde(rename = "terminal.ansi.blue")]
    pub terminal_ansi_blue: Option<ThemeColor>,

    /// Bright blue ANSI terminal color.
    #[serde(rename = "terminal.ansi.bright_blue")]
    pub terminal_ansi_bright_blue: Option<ThemeColor>,

    /// Dim blue ANSI terminal color.
    #[serde(rename = "terminal.ansi.dim_blue")]
    pub terminal_ansi_dim_blue: Option<ThemeColor>,

    /// Magenta ANSI terminal color.
    #[serde(rename = "terminal.ansi.magenta")]
    pub terminal_ansi_magenta: Option<ThemeColor>,

    /// Bright magenta ANSI terminal color.
    #[serde(rename = "terminal.ansi.bright_magenta")]
    pub terminal_ansi_bright_magenta: Option<ThemeColor>,

    /// Dim magenta ANSI terminal color.
    #[serde(rename = "terminal.ansi.dim_magenta")]
    pub terminal_ansi_dim_magenta: Option<ThemeColor>,

    /// Cyan ANSI terminal color.
    #[serde(rename = "terminal.ansi.cyan")]
    pub terminal_ansi_cyan: Option<ThemeColor>,

    /// Bright cyan ANSI terminal color.
    #[serde(rename = "terminal.ansi.bright_cyan")]
    pub terminal_ansi_bright_cyan: Option<ThemeColor>,

    /// Dim cyan ANSI terminal color.
    #[serde(rename = "terminal.ansi.dim_cyan")]
    pub terminal_ansi_dim_cyan: Option<ThemeColor>,

    /// White ANSI terminal color.
    #[serde(rename = "terminal.ansi.white")]
    pub terminal_ansi_white: Option<ThemeColor>,

    /// Bright white ANSI terminal color.
    #[serde(rename = "terminal.ansi.bright_white")]
    pub terminal_ansi_bright_white: Option<ThemeColor>,

    /// Dim white ANSI terminal color.
    #[serde(rename = "terminal.ansi.dim_white")]
    pub terminal_ansi_dim_white: Option<ThemeColor>,

    #[serde(rename = "link_text.hover")]
    pub link_text_hover: Option<ThemeColor>,

    /// Added version control color.
    #[serde(rename = "version_control.added")]
    pub version_control_added: Option<ThemeColor>,

    /// Deleted version control color.
    #[serde(rename = "version_control.deleted")]
    pub version_control_deleted: Option<ThemeColor>,

    /// Modified version control color.
    #[serde(rename = "version_control.modified")]
    pub version_control_modified: Option<ThemeColor>,

    /// Renamed version control color.
    #[serde(rename = "version_control.renamed")]
    pub version_control_renamed: Option<ThemeColor>,

    /// Conflict version control color.
    #[serde(rename = "version_control.conflict")]
    pub version_control_conflict: Option<ThemeColor>,

    /// Ignored version control color.
    #[serde(rename = "version_control.ignored")]
    pub version_control_ignored: Option<ThemeColor>,

    /// Color for added words in word diffs.
    #[serde(rename = "version_control.word_added")]
    pub version_control_word_added: Option<ThemeColor>,

    /// Color for deleted words in word diffs.
    #[serde(rename = "version_control.word_deleted")]
    pub version_control_word_deleted: Option<ThemeColor>,

    /// Background color for row highlights of "ours" regions in merge conflicts.
    #[serde(rename = "version_control.conflict_marker.ours")]
    pub version_control_conflict_marker_ours: Option<ThemeColor>,

    /// Background color for row highlights of "theirs" regions in merge conflicts.
    #[serde(rename = "version_control.conflict_marker.theirs")]
    pub version_control_conflict_marker_theirs: Option<ThemeColor>,

    /// Deprecated in favor of `version_control_conflict_marker_ours`.
    #[deprecated]
    pub version_control_conflict_ours_background: Option<ThemeColor>,

    /// Deprecated in favor of `version_control_conflict_marker_theirs`.
    #[deprecated]
    pub version_control_conflict_theirs_background: Option<ThemeColor>,

    /// Background color for Vim Normal mode indicator.
    #[serde(rename = "vim.normal.background")]
    pub vim_normal_background: Option<ThemeColor>,
    /// Background color for Vim Insert mode indicator.
    #[serde(rename = "vim.insert.background")]
    pub vim_insert_background: Option<ThemeColor>,
    /// Background color for Vim Replace mode indicator.
    #[serde(rename = "vim.replace.background")]
    pub vim_replace_background: Option<ThemeColor>,
    /// Background color for Vim Visual mode indicator.
    #[serde(rename = "vim.visual.background")]
    pub vim_visual_background: Option<ThemeColor>,
    /// Background color for Vim Visual Line mode indicator.
    #[serde(rename = "vim.visual_line.background")]
    pub vim_visual_line_background: Option<ThemeColor>,
    /// Background color for Vim Visual Block mode indicator.
    #[serde(rename = "vim.visual_block.background")]
    pub vim_visual_block_background: Option<ThemeColor>,
    /// Background color for Vim yank highlight.
    #[serde(rename = "vim.yank.background")]
    pub vim_yank_background: Option<ThemeColor>,
    /// Foreground color for Helix jump labels.
    #[serde(rename = "vim.helix_jump_label.foreground")]
    pub vim_helix_jump_label_foreground: Option<ThemeColor>,
    /// Background color for Vim Helix Normal mode indicator.
    #[serde(rename = "vim.helix_normal.background")]
    pub vim_helix_normal_background: Option<ThemeColor>,
    /// Background color for Vim Helix Select mode indicator.
    #[serde(rename = "vim.helix_select.background")]
    pub vim_helix_select_background: Option<ThemeColor>,
    /// Background color for Vim Normal mode indicator.
    #[serde(rename = "vim.normal.foreground")]
    pub vim_normal_foreground: Option<ThemeColor>,
    /// Foreground color for Vim Insert mode indicator.
    #[serde(rename = "vim.insert.foreground")]
    pub vim_insert_foreground: Option<ThemeColor>,
    /// Foreground color for Vim Replace mode indicator.
    #[serde(rename = "vim.replace.foreground")]
    pub vim_replace_foreground: Option<ThemeColor>,
    /// Foreground color for Vim Visual mode indicator.
    #[serde(rename = "vim.visual.foreground")]
    pub vim_visual_foreground: Option<ThemeColor>,
    /// Foreground color for Vim Visual Line mode indicator.
    #[serde(rename = "vim.visual_line.foreground")]
    pub vim_visual_line_foreground: Option<ThemeColor>,
    /// Foreground color for Vim Visual Block mode indicator.
    #[serde(rename = "vim.visual_block.foreground")]
    pub vim_visual_block_foreground: Option<ThemeColor>,
    /// Foreground color for Vim Helix Normal mode indicator.
    #[serde(rename = "vim.helix_normal.foreground")]
    pub vim_helix_normal_foreground: Option<ThemeColor>,
    /// Foreground color for Vim Helix Select mode indicator.
    #[serde(rename = "vim.helix_select.foreground")]
    pub vim_helix_select_foreground: Option<ThemeColor>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HighlightStyleContent {
    pub color: Option<ThemeColor>,

    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "treat_error_as_none"
    )]
    pub background_color: Option<ThemeColor>,

    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "treat_error_as_none"
    )]
    pub font_style: Option<FontStyleContent>,

    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "treat_error_as_none"
    )]
    pub font_weight: Option<FontWeightContent>,
}

impl HighlightStyleContent {
    pub fn is_empty(&self) -> bool {
        self.color.is_none()
            && self.background_color.is_none()
            && self.font_style.is_none()
            && self.font_weight.is_none()
    }
}

fn treat_error_as_none<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    let value: Value = Deserialize::deserialize(deserializer)?;
    Ok(T::deserialize(value).ok())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StatusColorsContent {
    /// Indicates some kind of conflict, like a file changed on disk while it was open, or
    /// merge conflicts in a Git repository.
    #[serde(rename = "conflict")]
    pub conflict: Option<ThemeColor>,

    #[serde(rename = "conflict.background")]
    pub conflict_background: Option<ThemeColor>,

    #[serde(rename = "conflict.border")]
    pub conflict_border: Option<ThemeColor>,

    /// Indicates something new, like a new file added to a Git repository.
    #[serde(rename = "created")]
    pub created: Option<ThemeColor>,

    #[serde(rename = "created.background")]
    pub created_background: Option<ThemeColor>,

    #[serde(rename = "created.border")]
    pub created_border: Option<ThemeColor>,

    /// Indicates that something no longer exists, like a deleted file.
    #[serde(rename = "deleted")]
    pub deleted: Option<ThemeColor>,

    #[serde(rename = "deleted.background")]
    pub deleted_background: Option<ThemeColor>,

    #[serde(rename = "deleted.border")]
    pub deleted_border: Option<ThemeColor>,

    /// Indicates a system error, a failed operation or a diagnostic error.
    #[serde(rename = "error")]
    pub error: Option<ThemeColor>,

    #[serde(rename = "error.background")]
    pub error_background: Option<ThemeColor>,

    #[serde(rename = "error.border")]
    pub error_border: Option<ThemeColor>,

    /// Represents a hidden status, such as a file being hidden in a file tree.
    #[serde(rename = "hidden")]
    pub hidden: Option<ThemeColor>,

    #[serde(rename = "hidden.background")]
    pub hidden_background: Option<ThemeColor>,

    #[serde(rename = "hidden.border")]
    pub hidden_border: Option<ThemeColor>,

    /// Indicates a hint or some kind of additional information.
    #[serde(rename = "hint")]
    pub hint: Option<ThemeColor>,

    #[serde(rename = "hint.background")]
    pub hint_background: Option<ThemeColor>,

    #[serde(rename = "hint.border")]
    pub hint_border: Option<ThemeColor>,

    /// Indicates that something is deliberately ignored, such as a file or operation ignored by Git.
    #[serde(rename = "ignored")]
    pub ignored: Option<ThemeColor>,

    #[serde(rename = "ignored.background")]
    pub ignored_background: Option<ThemeColor>,

    #[serde(rename = "ignored.border")]
    pub ignored_border: Option<ThemeColor>,

    /// Represents informational status updates or messages.
    #[serde(rename = "info")]
    pub info: Option<ThemeColor>,

    #[serde(rename = "info.background")]
    pub info_background: Option<ThemeColor>,

    #[serde(rename = "info.border")]
    pub info_border: Option<ThemeColor>,

    /// Indicates a changed or altered status, like a file that has been edited.
    #[serde(rename = "modified")]
    pub modified: Option<ThemeColor>,

    #[serde(rename = "modified.background")]
    pub modified_background: Option<ThemeColor>,

    #[serde(rename = "modified.border")]
    pub modified_border: Option<ThemeColor>,

    /// Indicates something that is predicted, like automatic code completion, or generated code.
    #[serde(rename = "predictive")]
    pub predictive: Option<ThemeColor>,

    #[serde(rename = "predictive.background")]
    pub predictive_background: Option<ThemeColor>,

    #[serde(rename = "predictive.border")]
    pub predictive_border: Option<ThemeColor>,

    /// Represents a renamed status, such as a file that has been renamed.
    #[serde(rename = "renamed")]
    pub renamed: Option<ThemeColor>,

    #[serde(rename = "renamed.background")]
    pub renamed_background: Option<ThemeColor>,

    #[serde(rename = "renamed.border")]
    pub renamed_border: Option<ThemeColor>,

    /// Indicates a successful operation or task completion.
    #[serde(rename = "success")]
    pub success: Option<ThemeColor>,

    #[serde(rename = "success.background")]
    pub success_background: Option<ThemeColor>,

    #[serde(rename = "success.border")]
    pub success_border: Option<ThemeColor>,

    /// Indicates some kind of unreachable status, like a block of code that can never be reached.
    #[serde(rename = "unreachable")]
    pub unreachable: Option<ThemeColor>,

    #[serde(rename = "unreachable.background")]
    pub unreachable_background: Option<ThemeColor>,

    #[serde(rename = "unreachable.border")]
    pub unreachable_border: Option<ThemeColor>,

    /// Represents a warning status, like an operation that is about to fail.
    #[serde(rename = "warning")]
    pub warning: Option<ThemeColor>,

    #[serde(rename = "warning.background")]
    pub warning_background: Option<ThemeColor>,

    #[serde(rename = "warning.border")]
    pub warning_border: Option<ThemeColor>,
}

/// The background appearance of the window.
#[derive(Debug, PartialEq, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowBackgroundContent {
    Opaque,
    Transparent,
    Blurred,
    MicaBackdrop,
    MicaAltBackdrop,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FontStyleContent {
    Normal,
    Italic,
    Oblique,
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FontWeightContent(pub f32);

impl Display for FontWeightContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<f32> for FontWeightContent {
    fn from(weight: f32) -> Self {
        FontWeightContent(weight)
    }
}

impl Default for FontWeightContent {
    fn default() -> Self {
        Self::NORMAL
    }
}

impl FontWeightContent {
    pub const THIN: FontWeightContent = FontWeightContent(100.0);
    pub const EXTRA_LIGHT: FontWeightContent = FontWeightContent(200.0);
    pub const LIGHT: FontWeightContent = FontWeightContent(300.0);
    pub const NORMAL: FontWeightContent = FontWeightContent(400.0);
    pub const MEDIUM: FontWeightContent = FontWeightContent(500.0);
    pub const SEMIBOLD: FontWeightContent = FontWeightContent(600.0);
    pub const BOLD: FontWeightContent = FontWeightContent(700.0);
    pub const EXTRA_BOLD: FontWeightContent = FontWeightContent(800.0);
    pub const BLACK: FontWeightContent = FontWeightContent(900.0);
}
