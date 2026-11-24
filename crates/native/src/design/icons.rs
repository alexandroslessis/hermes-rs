//! Icons: Lucide, as t3code uses it. GPUI Kit's default bundle (`Assets`) holds 101 of them (the
//! caption buttons, panels, chevrons, search, settings…); the few more t3code's chrome and rows
//! use are compiled in here on top of it ([`AppAssets`], what every window and test registers as
//! its asset source). Adding an icon is a name in the list below.
//!
//! t3code: `apps/web/src/components/Icons.tsx` and the `lucide-react` imports of the sidebar,
//! the header and the buttons.

use std::borrow::Cow;

use gpui_kit::{
    AssetSource, Result, SharedString,
    assets::{Assets, IconName, icon_assets},
    component::Icon,
};

icon_assets!(
    ExtraIcons,
    [
        Archive,
        ArrowUp,
        Brain,
        ChevronsDownUp,
        ChartColumn,
        CircleDashed,
        CircleDot,
        CircleDotDashed,
        Clock,
        Command,
        CornerDownLeft,
        Download,
        FileDiff,
        FilePen,
        FileSearch,
        FolderPlus,
        GitBranch,
        Lightbulb,
        ListFilter,
        MessageSquare,
        MessageSquareWarning,
        OctagonAlert,
        Paperclip,
        Pencil,
        Pin,
        PinOff,
        RotateCcw,
        ShieldQuestionMark,
        SquarePen,
        Trash,
        Wrench,
    ]
);

/// GPUI Kit's default icons plus [`ExtraIcons`]: the app's asset source.
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

/// An icon of the shared Lucide catalogue, ready to size and colour.
pub fn icon(name: IconName) -> Icon {
    Icon::new(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_extra_icon_loads_and_the_defaults_still_do() {
        let assets = AppAssets;
        for name in [
            IconName::SquarePen,
            IconName::FolderPlus,
            IconName::ListFilter,
            IconName::GitBranch,
            IconName::Download,
            IconName::MessageSquare,
            IconName::Command,
            IconName::CircleDashed,
            IconName::CircleDot,
            IconName::CircleDotDashed,
            // From GPUI Kit's own bundle.
            IconName::Settings,
            IconName::PanelLeft,
            IconName::WindowClose,
        ] {
            let path = name.path();
            assert!(
                assets.load(&path).unwrap().is_some(),
                "{path} is not in the app's assets"
            );
        }
        // An unknown path is an error from the asset source, never a blank icon.
        assert!(assets.load("icons/not-an-icon.svg").is_err());
    }
}
