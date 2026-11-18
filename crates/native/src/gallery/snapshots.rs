//! The snapshot harness (`cargo test -p hermes-native --features snapshots`): each registered
//! story, in each of its states, in GPUI Kit's light and dark theme, rendered off-screen to one PNG
//! (the states side by side, each cropped to the story's element) and compared with the approved
//! image `crates/native/snapshots/<story>-<theme>.png`.
//!
//! - A difference writes `<name>.actual.png` and `<name>.diff.png` (changed pixels red over a
//!   faded baseline) into `<target>/snapshots/` (`target/claude/snapshots/` for us) and fails,
//!   printing `snapshot-diff: <path>` per image for `scripts/native/check.ps1`.
//! - `UPDATE_SNAPSHOTS=1` accepts what was rendered as the new baseline.
//! - A story with no baseline fails the same way, with only `<name>.actual.png`.
//!
//! Rendering: GPUI's test platform with a real renderer, `gpui-pre-wgpu`'s `WgpuHeadlessRenderer`
//! (`gpui-pre-platform` wires it on Linux only; it builds and runs on Windows over Vulkan or
//! DirectX 12), and the real DirectWrite text system (`crate::testing::text_system`). Test windows
//! have a scale factor of 2, so images are at twice the logical size. No window is shown.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui_kit::{HeadlessAppContext, PlatformHeadlessRenderer};
use image::{Rgba, RgbaImage};

use super::{Story, all, button, open_story};
use crate::testing::{Layout, text_system};
use crate::theme::ThemeChoice;

/// A pixel counts as changed when any channel moved by more than this (GPU rasterisation of
/// anti-aliased edges and glyphs may differ by a step or two between drivers).
const CHANNEL_TOLERANCE: u8 = 8;
/// The themes every story is snapshotted in.
const THEMES: [ThemeChoice; 2] = [ThemeChoice::DefaultLight, ThemeChoice::DefaultDark];
/// Space between two states in a story's image, filled with [`GAP_COLOR`].
const GAP: u32 = 4;
const GAP_COLOR: Rgba<u8> = Rgba([255, 0, 255, 255]);

/// A headless app that can render: real text, GPUI Kit's assets, the wgpu offscreen renderer.
fn rendering_app() -> HeadlessAppContext {
    let mut cx = HeadlessAppContext::with_platform(
        text_system(),
        Arc::new(gpui_kit::assets::Assets),
        || {
            gpui_wgpu::WgpuHeadlessRenderer::new()
                .map(|renderer| Some(Box::new(renderer) as Box<dyn PlatformHeadlessRenderer>))
        },
    );
    cx.update(gpui_kit::init);
    cx
}

/// `story` in every state under `theme`: each state cropped to the `story` element, left to
/// right, [`GAP`] pixels apart.
fn render_story(
    cx: &mut HeadlessAppContext,
    story: &'static Story,
    theme: ThemeChoice,
) -> RgbaImage {
    let crops: Vec<RgbaImage> = (0..story.states.len())
        .map(|state| {
            let window = open_story(cx, story, state, theme);
            let (image, bounds, scale) = cx
                .update_window(window, |_, window, _| {
                    let image = window.render_to_image();
                    (
                        image,
                        Layout::of(window).bounds("story"),
                        window.scale_factor(),
                    )
                })
                .expect("read the story window");
            let image = image.expect("render the story off-screen");
            crop(&image, bounds, scale)
        })
        .collect();
    let width = crops.iter().map(RgbaImage::width).sum::<u32>()
        + GAP * (crops.len() as u32).saturating_sub(1);
    let height = crops.iter().map(RgbaImage::height).max().unwrap_or(0);
    let mut sheet = RgbaImage::from_pixel(width, height, GAP_COLOR);
    let mut x = 0;
    for crop in &crops {
        image::imageops::replace(&mut sheet, crop, i64::from(x), 0);
        x += crop.width() + GAP;
    }
    sheet
}

/// The part of `image` (device pixels) under `bounds` (logical pixels).
fn crop(image: &RgbaImage, bounds: gpui_kit::Bounds<gpui_kit::Pixels>, scale: f32) -> RgbaImage {
    let to_px = |v: gpui_kit::Pixels| (v.as_f32() * scale).round().max(0.) as u32;
    let x = to_px(bounds.origin.x).min(image.width());
    let y = to_px(bounds.origin.y).min(image.height());
    let w = to_px(bounds.size.width).min(image.width() - x);
    let h = to_px(bounds.size.height).min(image.height() - y);
    image::imageops::crop_imm(image, x, y, w, h).to_image()
}

/// How `actual` differs from `baseline`: the number of changed pixels (all of them when the sizes
/// differ) and an image showing where.
fn compare(baseline: &RgbaImage, actual: &RgbaImage) -> (u64, RgbaImage) {
    let (w, h) = (
        baseline.width().max(actual.width()),
        baseline.height().max(actual.height()),
    );
    let mut diff = RgbaImage::new(w, h);
    let mut changed = 0;
    for (x, y, out) in diff.enumerate_pixels_mut() {
        let before = baseline.get_pixel_checked(x, y);
        let after = actual.get_pixel_checked(x, y);
        let same = match (before, after) {
            (Some(a), Some(b)) => {
                a.0.iter()
                    .zip(b.0)
                    .all(|(p, q)| p.abs_diff(q) <= CHANNEL_TOLERANCE)
            }
            _ => false,
        };
        *out = if same {
            // The baseline, faded towards white, so the red stands out.
            let p = before.copied().unwrap_or(Rgba([255; 4])).0;
            let fade = |c: u8| (u16::from(c) * 3 / 10 + 255 * 7 / 10) as u8;
            Rgba([fade(p[0]), fade(p[1]), fade(p[2]), 255])
        } else {
            changed += 1;
            Rgba([255, 0, 0, 255])
        };
    }
    if baseline.dimensions() != actual.dimensions() {
        changed = u64::from(w) * u64::from(h);
    }
    (changed, diff)
}

/// `crates/native/snapshots/`.
fn baseline_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("snapshots")
}

/// `<target>/snapshots/`: the test binary is `<target>/<profile>/deps/<name>.exe`.
fn output_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("the test binary's path");
    let target = exe
        .ancestors()
        .nth(3)
        .expect("the test binary lives under <target>/<profile>/deps");
    target.join("snapshots")
}

#[core::prelude::v1::test]
fn every_story_matches_its_snapshot() {
    let update = std::env::var("UPDATE_SNAPSHOTS").is_ok_and(|v| v == "1");
    let out = output_dir();
    std::fs::create_dir_all(&out).expect("create the snapshot output folder");
    let mut cx = rendering_app();
    let mut failures = Vec::new();
    for story in all() {
        for theme in THEMES {
            let name = format!("{}-{}", story.name, theme.slug());
            let actual = render_story(&mut cx, story, theme);
            let baseline_path = baseline_dir().join(format!("{name}.png"));
            let actual_path = out.join(format!("{name}.actual.png"));
            let diff_path = out.join(format!("{name}.diff.png"));
            let _ = std::fs::remove_file(&actual_path);
            let _ = std::fs::remove_file(&diff_path);
            if update {
                std::fs::create_dir_all(baseline_dir()).expect("create crates/native/snapshots");
                actual.save(&baseline_path).expect("write the baseline");
                println!("snapshot-accepted: {}", baseline_path.display());
                continue;
            }
            let baseline = match image::open(&baseline_path) {
                Ok(baseline) => baseline.to_rgba8(),
                Err(_) => {
                    actual.save(&actual_path).expect("write the actual image");
                    println!("snapshot-new: {}", actual_path.display());
                    failures.push(format!(
                        "{name}: no baseline at {} (accept with UPDATE_SNAPSHOTS=1); rendered: {}",
                        baseline_path.display(),
                        actual_path.display()
                    ));
                    continue;
                }
            };
            let (changed, diff) = compare(&baseline, &actual);
            if changed > 0 {
                actual.save(&actual_path).expect("write the actual image");
                diff.save(&diff_path).expect("write the diff image");
                println!("snapshot-diff: {}", diff_path.display());
                failures.push(format!(
                    "{name}: {changed} pixels differ ({}x{} -> {}x{}); diff: {}; actual: {}",
                    baseline.width(),
                    baseline.height(),
                    actual.width(),
                    actual.height(),
                    diff_path.display(),
                    actual_path.display()
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} snapshot(s) differ:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

/// The same story rendered twice gives the same pixels, so a difference means a change.
#[core::prelude::v1::test]
fn rendering_a_story_twice_gives_the_same_pixels() {
    let mut cx = rendering_app();
    let first = render_story(&mut cx, &button::STATES_STORY, ThemeChoice::DefaultLight);
    let second = render_story(&mut cx, &button::STATES_STORY, ThemeChoice::DefaultLight);
    let (changed, _) = compare(&first, &second);
    assert_eq!(changed, 0);
    // And the image is not blank: the five states differ from one another.
    let distinct: std::collections::HashSet<_> = first.pixels().map(|p| p.0).collect();
    assert!(
        distinct.len() > 20,
        "only {} colours rendered",
        distinct.len()
    );
}

/// The deliberately broken variant (the button squeezed too narrow for its label) does not match
/// the real story's image.
#[core::prelude::v1::test]
fn a_squeezed_button_is_caught_by_the_snapshot_comparison() {
    let mut cx = rendering_app();
    let good = render_story(&mut cx, &button::STATES_STORY, ThemeChoice::DefaultLight);
    let broken = render_story(&mut cx, &button::SQUEEZED_STORY, ThemeChoice::DefaultLight);
    let (changed, _) = compare(&good, &broken);
    assert!(changed > 100, "only {changed} pixels changed");
}
