//! Themes: the list of themes the app can wear (t3code's light and dark, the Zed families
//! bundled in `assets/themes/`, and the user's files in `<data>/themes/`), the one in use, and
//! putting a theme onto GPUI Kit so every component follows it.
//!
//! A theme is a [`ThemeDef`]: a name, light or dark, and a [`Palette`] of our tokens
//! ([`crate::design::tokens`]). t3code's two are built in ([`ThemeDef::default_light`],
//! [`ThemeDef::default_dark`]); a Zed theme file maps onto the same tokens ([`zed`]); [`kit`] turns
//! a palette into GPUI Kit's theme config; [`apply`] makes it the active theme. Views read colours
//! from [`DesignTheme::palette`] (our tokens) or, for Kit's own components, from Kit's `Theme`,
//! which [`apply`] keeps in step.
//!
//! The user's folder is read at start ([`Themes::load`]) and watched ([`watch_user_themes`]); a
//! theme that disappears while it is in use keeps being worn until another is picked.
//!
//! t3code: `apps/web/src/index.css` (the two themes), `apps/web/src/appearanceFonts.ts` (the font
//! stacks).

pub mod folder;
mod fonts;
pub mod kit;
pub mod zed;

use std::{path::PathBuf, rc::Rc};

use gpui_kit::{
    App, Global,
    component::{Theme, ThemeMode, ThemeRegistry},
};
use serde_json::{Map, Value};

pub use fonts::Fonts;

use crate::design::tokens::{Appearance, Palette};

/// The bundled Zed theme families, as Zed ships them: One (`One Dark`, `One Light`; Zed's port of
/// Atom's One, MIT, copyright GitHub Inc., `assets/themes/one/LICENSE`) and Ayu (MIT, copyright
/// Ike Ku, `assets/themes/ayu/LICENSE`).
const BUNDLED: &[(&str, &str)] = &[
    ("one", include_str!("../../assets/themes/one/one.json")),
    ("ayu", include_str!("../../assets/themes/ayu/ayu.json")),
];

/// Where a theme comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// t3code's light and dark, in the code.
    Builtin,
    /// A Zed family file shipped in the binary.
    Bundled,
    /// A file in the user's theme folder.
    User(PathBuf),
}

/// One theme.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeDef {
    /// The name shown and used to pick it. Names are unique in a [`Themes`] list.
    pub name: String,
    /// Light or dark: decides which GPUI Kit mode it uses.
    pub appearance: Appearance,
    /// The colours.
    pub palette: Palette,
    /// The code colours for GPUI Kit's `highlight` section (Zed's `style`); `None` keeps Kit's
    /// own for the appearance.
    pub highlight: Option<Map<String, Value>>,
    /// Where it came from.
    pub source: Source,
}

impl ThemeDef {
    /// t3code's light theme, the default for a light system.
    pub fn default_light() -> Self {
        Self {
            name: "Default Light".into(),
            appearance: Appearance::Light,
            palette: Palette::light(),
            highlight: None,
            source: Source::Builtin,
        }
    }

    /// t3code's dark theme, the default for a dark system.
    pub fn default_dark() -> Self {
        Self {
            name: "Default Dark".into(),
            appearance: Appearance::Dark,
            palette: Palette::dark(),
            highlight: None,
            source: Source::Builtin,
        }
    }
}

/// The themes available, in the order they are offered: built in, bundled, then the user's. A
/// later theme with the same name replaces an earlier one (a user can override a bundled theme).
#[derive(Clone, Debug)]
pub struct Themes {
    list: Vec<ThemeDef>,
}

impl Global for Themes {}

impl Themes {
    /// The built-in and bundled themes, then `user`'s.
    pub fn new(user: Vec<ThemeDef>) -> Self {
        let mut list = vec![ThemeDef::default_light(), ThemeDef::default_dark()];
        list.extend(bundled().iter().cloned());
        for theme in user {
            match list.iter().position(|t| t.name == theme.name) {
                Some(ix) => list[ix] = theme,
                None => list.push(theme),
            }
        }
        Self { list }
    }

    /// Reads the user's folder (`<data>/themes/`) and builds the list; problems are logged.
    pub fn load(data_dir: &std::path::Path) -> (Self, folder::Scan) {
        let scan = folder::scan(&folder::themes_dir(data_dir));
        (Self::new(scan.themes.clone()), scan)
    }

    /// The list installed in the app (empty built-ins only until [`Themes::install`] is called).
    pub fn global(cx: &App) -> Themes {
        cx.try_global::<Themes>()
            .cloned()
            .unwrap_or_else(|| Themes::new(Vec::new()))
    }

    /// Makes this the app's list (what [`watch_user_themes`] keeps up to date).
    pub fn install(self, cx: &mut App) {
        cx.set_global(self);
    }

    /// Every theme.
    pub fn all(&self) -> &[ThemeDef] {
        &self.list
    }

    /// The theme called `name`.
    pub fn find(&self, name: &str) -> Option<&ThemeDef> {
        self.list.iter().find(|t| t.name == name)
    }

    /// t3code's theme for `appearance`.
    pub fn default_for(&self, appearance: Appearance) -> &ThemeDef {
        let name = match appearance {
            Appearance::Light => "Default Light",
            Appearance::Dark => "Default Dark",
        };
        // The built-in names can be overridden by a user file but never removed.
        self.find(name).unwrap_or(&self.list[0])
    }
}

/// The bundled Zed themes, parsed once. A bundled file that fails to parse is a bug caught by the
/// tests; it is skipped here rather than taking the app down.
fn bundled() -> &'static [ThemeDef] {
    static BUNDLED_THEMES: std::sync::OnceLock<Vec<ThemeDef>> = std::sync::OnceLock::new();
    BUNDLED_THEMES.get_or_init(|| {
        let mut themes = Vec::new();
        for (name, json) in BUNDLED {
            match zed::parse_family(json) {
                Ok(family) => themes.extend(family.themes.into_iter().map(|theme| ThemeDef {
                    name: theme.name,
                    appearance: theme.appearance,
                    palette: theme.palette,
                    highlight: Some(theme.highlight),
                    source: Source::Bundled,
                })),
                Err(error) => tracing::error!("bundled theme file {name}: {error:#}"),
            }
        }
        themes
    })
}

/// The theme in use, as a global.
struct Active(ThemeDef);

impl Global for Active {}

/// Our tokens for the theme in use, from any `&App` (a `Context` derefs to one).
pub trait DesignTheme {
    /// The active theme (t3code's dark until one is applied).
    fn design_theme(&self) -> &ThemeDef;

    /// Its colours.
    fn palette(&self) -> &Palette {
        &self.design_theme().palette
    }
}

impl DesignTheme for App {
    fn design_theme(&self) -> &ThemeDef {
        static FALLBACK: std::sync::OnceLock<ThemeDef> = std::sync::OnceLock::new();
        match self.try_global::<Active>() {
            Some(active) => &active.0,
            None => FALLBACK.get_or_init(ThemeDef::default_dark),
        }
    }
}

/// Makes `def` the app's theme: our tokens (for [`DesignTheme`]) and GPUI Kit's theme, so Kit's
/// components follow. Fonts are resolved against what is installed. A theme Kit rejects is logged
/// and leaves the current one in place.
pub fn apply(def: &ThemeDef, cx: &mut App) {
    KitBaseline::remember(cx);
    let fonts = Fonts::install(cx);
    let mut config = match kit::config(def, Some(&fonts)) {
        Ok(config) => config,
        Err(error) => {
            tracing::error!("theme {}: {error:#}", def.name);
            return;
        }
    };
    // A theme without its own code colours gets Kit's for the appearance, not the previous
    // theme's (Kit keeps its highlight theme until a config replaces it).
    if config.highlight.is_none() {
        let registry = ThemeRegistry::global(cx);
        let kit_default = if def.appearance.is_dark() {
            registry.default_dark_theme()
        } else {
            registry.default_light_theme()
        };
        config.highlight = kit_default.highlight.clone();
    }
    Theme::update(cx, |theme| theme.apply_config(&Rc::new(config)));
    cx.set_global(Active(def.clone()));
}

/// GPUI Kit's own font, size and radius settings, as they were before [`apply`] first replaced
/// them. Kit's `apply_config` only writes the fields a config sets and Kit's default themes set
/// none of these, so [`apply_kit_default`] puts them back from here.
#[derive(Clone)]
struct KitBaseline {
    font_family: gpui_kit::SharedString,
    font_size: gpui_kit::Pixels,
    mono_font_family: gpui_kit::SharedString,
    mono_font_size: gpui_kit::Pixels,
    radius: gpui_kit::Pixels,
    radius_lg: gpui_kit::Pixels,
    highlight_theme: std::sync::Arc<gpui_kit::component::highlighter::HighlightTheme>,
}

impl Global for KitBaseline {}

impl KitBaseline {
    /// Records Kit's settings the first time; later calls keep the first record.
    fn remember(cx: &mut App) {
        if cx.has_global::<KitBaseline>() || !cx.has_global::<Theme>() {
            return;
        }
        let theme = Theme::global(cx);
        let baseline = KitBaseline {
            font_family: theme.font_family.clone(),
            font_size: theme.font_size,
            mono_font_family: theme.mono_font_family.clone(),
            mono_font_size: theme.mono_font_size,
            radius: theme.radius,
            radius_lg: theme.radius_lg,
            highlight_theme: theme.highlight_theme.clone(),
        };
        cx.set_global(baseline);
    }
}

/// Applies GPUI Kit's own theme for `appearance` (the CP0.2 gallery's "GPUI Kit" family), with
/// Kit's own fonts, sizes, radii and code colours; our tokens are left as they were.
pub fn apply_kit_default(appearance: Appearance, cx: &mut App) {
    // Put GPUI Kit's own themes back before switching mode, or the mode keeps the colours of the
    // last theme applied.
    let registry = ThemeRegistry::global(cx);
    let light = registry.default_light_theme().clone();
    let dark = registry.default_dark_theme().clone();
    Theme::update(cx, |theme| {
        theme.light_theme = light;
        theme.dark_theme = dark;
    });
    let mode = if appearance.is_dark() {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    Theme::change(mode, None, cx);
    if let Some(baseline) = cx.try_global::<KitBaseline>().cloned() {
        Theme::update(cx, |theme| {
            theme.font_family = baseline.font_family;
            theme.font_size = baseline.font_size;
            theme.mono_font_family = baseline.mono_font_family;
            theme.mono_font_size = baseline.mono_font_size;
            theme.radius = baseline.radius;
            theme.radius_lg = baseline.radius_lg;
            theme.highlight_theme = baseline.highlight_theme;
        });
    }
}

/// Replaces the themes list with `scan`'s files after the folder changed, and re-applies the
/// theme in use when its definition changed on disk.
fn reload(scan: folder::Scan, cx: &mut App) {
    for problem in &scan.problems {
        tracing::warn!("theme file {} ignored: {}", problem.file, problem.reason);
    }
    let themes = Themes::new(scan.themes);
    let current = cx.design_theme().name.clone();
    let updated = themes.find(&current).cloned();
    cx.set_global(themes);
    if let Some(def) = updated
        && def != *cx.design_theme()
    {
        apply(&def, cx);
    }
}

/// The polling task of [`watch_user_themes`]. Held as a global so it ends with the app (and a
/// second call replaces the first instead of polling twice); dropping a GPUI task cancels it.
struct UserThemeWatch(#[allow(dead_code)] gpui_kit::Task<()>);

impl Global for UserThemeWatch {}

/// Watches the user's theme folder for the life of the app: when a file is added, edited or
/// removed, the themes list is rebuilt and the active theme re-applied if its file changed.
/// `seen` is the [`folder::Scan::fingerprint`] of the scan the list was built from. The folder
/// is looked at on a background thread; only the rebuild runs on the main thread. Calling it
/// again stops the previous watch.
pub fn watch_user_themes(dir: PathBuf, seen: folder::Fingerprint, cx: &mut App) {
    let mut watcher = folder::Watcher::new(dir, seen);
    let task = cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(folder::POLL_EVERY).await;
            let (back, scan) = cx
                .background_executor()
                .spawn(async move {
                    let scan = watcher.poll();
                    (watcher, scan)
                })
                .await;
            watcher = back;
            if let Some(scan) = scan {
                cx.update(|cx| reload(scan, cx));
            }
        }
    });
    cx.set_global(UserThemeWatch(task));
}

/// A theme to switch to in the spike and the gallery: GPUI Kit's own, Zed's One, or t3code's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeChoice {
    /// GPUI Kit's default dark theme.
    DefaultDark,
    /// GPUI Kit's default light theme.
    DefaultLight,
    /// Zed's One Dark, mapped through [`zed`].
    OneDark,
    /// Zed's One Light, mapped through [`zed`].
    OneLight,
    /// t3code's dark theme (the app's default).
    T3Dark,
    /// t3code's light theme.
    T3Light,
}

impl ThemeChoice {
    /// Every theme, in the order the spike lists them.
    pub const ALL: [ThemeChoice; 6] = [
        Self::DefaultDark,
        Self::DefaultLight,
        Self::OneDark,
        Self::OneLight,
        Self::T3Dark,
        Self::T3Light,
    ];

    /// The name shown on the theme's button.
    pub fn label(self) -> &'static str {
        match self {
            Self::DefaultDark => "GPUI Kit dark",
            Self::DefaultLight => "GPUI Kit light",
            Self::OneDark => "One Dark",
            Self::OneLight => "One Light",
            Self::T3Dark => "Default Dark",
            Self::T3Light => "Default Light",
        }
    }

    /// The short name used in snapshot file names (`button-states-light.png`): the GPUI Kit
    /// themes keep `light` and `dark`, the others are prefixed.
    pub fn slug(self) -> &'static str {
        match self {
            Self::DefaultDark => "dark",
            Self::DefaultLight => "light",
            Self::OneDark => "one-dark",
            Self::OneLight => "one-light",
            Self::T3Dark => "t3-dark",
            Self::T3Light => "t3-light",
        }
    }

    /// Whether it is a dark theme.
    pub fn is_dark(self) -> bool {
        matches!(self, Self::DefaultDark | Self::OneDark | Self::T3Dark)
    }

    /// Whether it is one of Zed's (One).
    pub fn is_zed(self) -> bool {
        matches!(self, Self::OneDark | Self::OneLight)
    }

    /// Whether it is t3code's.
    pub fn is_t3(self) -> bool {
        matches!(self, Self::T3Dark | Self::T3Light)
    }

    /// The same family (GPUI Kit, Zed One or t3code) in light or dark.
    pub fn with_dark(self, dark: bool) -> Self {
        match (self.is_zed(), self.is_t3(), dark) {
            (true, _, false) => Self::OneLight,
            (true, _, true) => Self::OneDark,
            (_, true, false) => Self::T3Light,
            (_, true, true) => Self::T3Dark,
            (_, _, false) => Self::DefaultLight,
            (_, _, true) => Self::DefaultDark,
        }
    }

    /// The theme's name in a [`Themes`] list, for the choices that are in it.
    fn list_name(self) -> Option<&'static str> {
        match self {
            Self::OneDark => Some("One Dark"),
            Self::OneLight => Some("One Light"),
            Self::T3Dark => Some("Default Dark"),
            Self::T3Light => Some("Default Light"),
            _ => None,
        }
    }
}

/// Switches the whole app to `choice` (from the installed [`Themes`] list, so a user file that
/// overrides the theme wins).
pub fn apply_theme(choice: ThemeChoice, cx: &mut App) {
    match choice.list_name() {
        Some(name) => {
            let def = match cx.try_global::<Themes>() {
                Some(themes) => themes.find(name).cloned(),
                None => Themes::new(Vec::new()).find(name).cloned(),
            };
            match def {
                Some(def) => apply(&def, cx),
                None => tracing::error!("no theme named {name}"),
            }
        }
        None => apply_kit_default(
            if choice.is_dark() {
                Appearance::Dark
            } else {
                Appearance::Light
            },
            cx,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_has_the_defaults_first_then_the_bundled_families() {
        let themes = Themes::new(Vec::new());
        let names: Vec<_> = themes.all().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(&names[..2], ["Default Light", "Default Dark"]);
        assert!(names.contains(&"One Dark") && names.contains(&"One Light"));
        assert!(names.contains(&"Ayu Dark"), "{names:?}");
        assert_eq!(
            themes.default_for(Appearance::Dark).palette,
            Palette::dark()
        );
        assert_eq!(
            themes.default_for(Appearance::Light).source,
            Source::Builtin
        );
        // Names are unique.
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
    }

    #[test]
    fn a_user_theme_with_a_known_name_replaces_it_and_a_new_one_is_added() {
        let mut mine = ThemeDef::default_dark();
        mine.palette.background = crate::design::tokens::Color::rgb(0x101010);
        mine.source = Source::User("mine.json".into());
        let mut other = mine.clone();
        other.name = "Mine".into();
        let themes = Themes::new(vec![mine, other]);
        assert_eq!(
            themes
                .default_for(Appearance::Dark)
                .palette
                .background
                .hex(),
            "#101010"
        );
        assert!(themes.find("Mine").is_some());
        assert_eq!(themes.all().len(), Themes::new(Vec::new()).all().len() + 1);
    }

    #[test]
    fn every_bundled_theme_becomes_a_gpui_kit_theme() {
        for theme in Themes::new(Vec::new()).all() {
            let config =
                kit::config(theme, None).unwrap_or_else(|e| panic!("{}: {e:#}", theme.name));
            assert_eq!(
                config.mode.is_dark(),
                theme.appearance.is_dark(),
                "{}",
                theme.name
            );
            assert_eq!(config.name.as_ref(), theme.name);
        }
    }

    #[test]
    fn the_choice_families_switch_between_light_and_dark() {
        for choice in ThemeChoice::ALL {
            assert!(choice.with_dark(true).is_dark());
            assert!(!choice.with_dark(false).is_dark());
            assert_eq!(choice.with_dark(true).is_zed(), choice.is_zed());
            assert_eq!(choice.with_dark(true).is_t3(), choice.is_t3());
        }
        let slugs: std::collections::HashSet<_> = ThemeChoice::ALL.map(ThemeChoice::slug).into();
        assert_eq!(slugs.len(), ThemeChoice::ALL.len());
    }

    fn kit_color(hex: &str) -> gpui_kit::Hsla {
        gpui_kit::component::try_parse_color(hex).expect("a colour")
    }

    #[gpui_kit::test]
    fn applying_a_theme_sets_our_tokens_and_gpui_kits(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::component::ActiveTheme as _;
        cx.update(|cx| {
            gpui_kit::init(cx);
            // Before anything is applied the tokens are t3code's dark.
            assert_eq!(cx.palette(), &Palette::dark());

            apply_theme(ThemeChoice::T3Light, cx);
            assert_eq!(cx.palette(), &Palette::light());
            assert!(!cx.theme().is_dark());
            assert_eq!(cx.theme().background, kit_color("#fcfcfc"));
            assert_eq!(cx.theme().border, kit_color("#e4e4e7"));

            apply_theme(ThemeChoice::T3Dark, cx);
            assert_eq!(cx.palette(), &Palette::dark());
            assert!(cx.theme().is_dark());
            assert_eq!(cx.theme().background, kit_color("#000000"));
            assert_eq!(cx.theme().muted_foreground, kit_color("#a1a1a1"));
            // The fonts are installed ones (or GPUI's own), never an empty name.
            assert!(!cx.theme().font_family.is_empty());
        });
    }

    #[gpui_kit::test]
    fn a_theme_without_code_colours_does_not_keep_the_previous_themes(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::component::ActiveTheme as _;
        cx.update(|cx| {
            gpui_kit::init(cx);
            apply_theme(ThemeChoice::OneDark, cx);
            let one = cx.theme().highlight_theme.style.editor_background;
            assert_eq!(
                one,
                Some(kit_color("#282c33ff")),
                "One Dark's code background"
            );
            apply_theme(ThemeChoice::T3Dark, cx);
            assert_ne!(cx.theme().highlight_theme.style.editor_background, one);
        });
    }

    #[gpui_kit::test]
    fn the_user_folder_is_watched_and_the_active_theme_follows_its_file(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let dir = hermes_testdir::tempdir().expect("temp folder");
        let themes_dir = folder::themes_dir(dir.path());
        std::fs::create_dir_all(&themes_dir).unwrap();
        let file = themes_dir.join("mine.json");
        let theme = |name: &str, background: &str| {
            format!(
                r##"{{"themes": [{{"name": "{name}", "appearance": "dark", "style": {{"editor.background": "{background}"}}}}]}}"##
            )
        };

        let (list, scan) = Themes::load(dir.path());
        assert!(list.find("Mine").is_none());
        cx.update(|cx| {
            gpui_kit::init(cx);
            list.install(cx);
            watch_user_themes(themes_dir.clone(), scan.fingerprint, cx);
        });

        // A file appears: its theme joins the list.
        std::fs::write(&file, theme("Mine", "#102030ff")).unwrap();
        cx.executor().advance_clock(folder::POLL_EVERY * 2);
        cx.run_until_parked();
        let mine = cx
            .update(|cx| Themes::global(cx).find("Mine").cloned())
            .expect("picked up");
        assert_eq!(mine.palette.background.hex(), "#102030");

        // It is worn, then edited: the window follows without a restart.
        cx.update(|cx| apply(&mine, cx));
        std::fs::write(&file, theme("Mine", "#405060ff")).unwrap();
        cx.executor().advance_clock(folder::POLL_EVERY * 2);
        cx.run_until_parked();
        cx.update(|cx| {
            use gpui_kit::component::ActiveTheme as _;
            assert_eq!(cx.palette().background.hex(), "#405060");
            assert_eq!(cx.theme().background, kit_color("#405060"));
        });

        // It is deleted: the list drops it, the window keeps wearing it until another is picked.
        std::fs::remove_file(&file).unwrap();
        cx.executor().advance_clock(folder::POLL_EVERY * 2);
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(Themes::global(cx).find("Mine").is_none());
            assert_eq!(cx.design_theme().name, "Mine");
        });

        // Dropping the watch (what quitting does) stops the polling: a new file is not seen.
        cx.update(|cx| drop(cx.remove_global::<UserThemeWatch>()));
        std::fs::write(themes_dir.join("late.json"), theme("Late", "#000000ff")).unwrap();
        cx.executor().advance_clock(folder::POLL_EVERY * 3);
        cx.run_until_parked();
        cx.update(|cx| assert!(Themes::global(cx).find("Late").is_none()));
    }

    #[gpui_kit::test]
    fn gpui_kits_own_theme_gets_its_own_fonts_and_radii_back(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::component::ActiveTheme as _;
        cx.update(|cx| {
            gpui_kit::init(cx);
            let kit = |cx: &App| {
                let t = cx.theme();
                (t.font_size, t.mono_font_size, t.radius, t.radius_lg)
            };
            apply_kit_default(Appearance::Dark, cx);
            let before = kit(cx);
            let font = cx.theme().font_family.clone();
            apply_theme(ThemeChoice::T3Dark, cx);
            assert_ne!(kit(cx), before, "t3code's sizes are not Kit's");
            apply_kit_default(Appearance::Dark, cx);
            assert_eq!(kit(cx), before);
            assert_eq!(cx.theme().font_family, font);
        });
    }
}
