//! Box-drawing and block characters drawn as rectangles on the cell grid instead of glyphs, so
//! frames from TUIs (`htop`, `vim`'s splits, tables) join up crisply from cell to cell whatever the
//! font's own glyphs look like. Lines (light, heavy, double; rounded corners drawn square), half
//! lines, the full and partial blocks and the three shades. Anything else is drawn by the font.
//!
//! Pure geometry: [`shapes`] answers rectangles in cell-relative fractions; the view scales them
//! to the cell and paints them.

/// A rectangle in a cell: `(x, y, w, h)` as fractions of the cell's width and height, with an
/// opacity (the shades are a block at 25 / 50 / 75 %).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub alpha: f32,
}

/// The weight of one arm of a line character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum W {
    None,
    Light,
    Heavy,
    Double,
}

/// `(up, right, down, left)` arms of a line character.
fn arms(c: char) -> Option<[W; 4]> {
    use W::{Double as D, Heavy as H, Light as L, None as N};
    Some(match c {
        '─' => [N, L, N, L],
        '━' => [N, H, N, H],
        '│' => [L, N, L, N],
        '┃' => [H, N, H, N],
        '┌' | '╭' => [N, L, L, N],
        '┐' | '╮' => [N, N, L, L],
        '└' | '╰' => [L, L, N, N],
        '┘' | '╯' => [L, N, N, L],
        '┏' => [N, H, H, N],
        '┓' => [N, N, H, H],
        '┗' => [H, H, N, N],
        '┛' => [H, N, N, H],
        '├' => [L, L, L, N],
        '┤' => [L, N, L, L],
        '┬' => [N, L, L, L],
        '┴' => [L, L, N, L],
        '┼' => [L, L, L, L],
        '┣' => [H, H, H, N],
        '┫' => [H, N, H, H],
        '┳' => [N, H, H, H],
        '┻' => [H, H, N, H],
        '╋' => [H, H, H, H],
        '═' => [N, D, N, D],
        '║' => [D, N, D, N],
        '╔' => [N, D, D, N],
        '╗' => [N, N, D, D],
        '╚' => [D, D, N, N],
        '╝' => [D, N, N, D],
        '╠' => [D, D, D, N],
        '╣' => [D, N, D, D],
        '╦' => [N, D, D, D],
        '╩' => [D, D, N, D],
        '╬' => [D, D, D, D],
        '╴' => [N, N, N, L],
        '╵' => [L, N, N, N],
        '╶' => [N, L, N, N],
        '╷' => [N, N, L, N],
        '╸' => [N, N, N, H],
        '╹' => [H, N, N, N],
        '╺' => [N, H, N, N],
        '╻' => [N, N, H, N],
        _ => return None,
    })
}

/// Whether `c` is drawn here rather than by the font.
pub fn is_drawn(c: char) -> bool {
    arms(c).is_some() || block(c).is_some()
}

fn block(c: char) -> Option<Vec<Rect>> {
    let full = |x: f32, y: f32, w: f32, h: f32| Rect {
        x,
        y,
        w,
        h,
        alpha: 1.,
    };
    let shade = |alpha: f32| Rect {
        x: 0.,
        y: 0.,
        w: 1.,
        h: 1.,
        alpha,
    };
    let eighths = |n: f32| n / 8.;
    Some(vec![match c {
        '█' => full(0., 0., 1., 1.),
        '▀' => full(0., 0., 1., 0.5),
        '▄' => full(0., 0.5, 1., 0.5),
        '▌' => full(0., 0., 0.5, 1.),
        '▐' => full(0.5, 0., 0.5, 1.),
        '░' => shade(0.25),
        '▒' => shade(0.5),
        '▓' => shade(0.75),
        '▁'..='▇' => {
            let n = (c as u32 - '▁' as u32 + 1) as f32;
            full(0., 1. - eighths(n), 1., eighths(n))
        }
        '▉'..='▏' => {
            // Left blocks, seven eighths down to one eighth.
            let n = 7. - (c as u32 - '▉' as u32) as f32;
            full(0., 0., eighths(n), 1.)
        }
        '▔' => full(0., 0., 1., eighths(1.)),
        '▕' => full(1. - eighths(1.), 0., eighths(1.), 1.),
        _ => return None,
    }])
}

/// The rectangles of `c` in a cell of `cell_w` × `cell_h` pixels (`line` is the light stroke's
/// thickness in pixels), or `None` when the font draws it.
pub fn shapes(c: char, cell_w: f32, cell_h: f32, line: f32) -> Option<Vec<Rect>> {
    if let Some(rects) = block(c) {
        return Some(rects);
    }
    let [up, right, down, left] = arms(c)?;
    let px_w = |p: f32| p / cell_w;
    let px_h = |p: f32| p / cell_h;
    let stroke = |w: W| match w {
        W::Heavy => (line * 2.).round().max(2.),
        _ => line,
    };
    // The centre lines, snapped to whole pixels so neighbours meet exactly.
    let cx = ((cell_w - line) / 2.).round();
    let cy = ((cell_h - line) / 2.).round();
    let gap = line.max(1.) * 2.;
    let mut out = Vec::new();
    let mut push = |x: f32, y: f32, w: f32, h: f32| {
        out.push(Rect {
            x: px_w(x),
            y: px_h(y),
            w: px_w(w),
            h: px_h(h),
            alpha: 1.,
        });
    };
    // Vertical arms (up / down), then horizontal (left / right).
    for (weight, top, bottom) in [(up, 0., cy + line), (down, cy, cell_h)] {
        match weight {
            W::None => {}
            W::Double => {
                for dx in [-gap / 2., gap / 2.] {
                    push((cx + dx).round(), top, line, bottom - top);
                }
            }
            w => {
                let s = stroke(w);
                push((cx - (s - line) / 2.).round(), top, s, bottom - top);
            }
        }
    }
    for (weight, from, to) in [(left, 0., cx + line), (right, cx, cell_w)] {
        match weight {
            W::None => {}
            W::Double => {
                for dy in [-gap / 2., gap / 2.] {
                    push(from, (cy + dy).round(), to - from, line);
                }
            }
            w => {
                let s = stroke(w);
                push(from, (cy - (s - line) / 2.).round(), to - from, s);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_reach_the_cell_edges_and_meet_in_the_middle() {
        // A horizontal line spans the whole width (so neighbours join), one stroke high.
        let h = shapes('─', 8., 16., 1.).unwrap();
        let x0 = h.iter().map(|r| r.x).fold(1., f32::min);
        let x1 = h.iter().map(|r| r.x + r.w).fold(0., f32::max);
        assert_eq!((x0, x1), (0., 1.));
        assert!(h.iter().all(|r| (r.h * 16.).round() == 1.));
        // A vertical line spans the whole height.
        let v = shapes('│', 8., 16., 1.).unwrap();
        let y1 = v.iter().map(|r| r.y + r.h).fold(0., f32::max);
        assert_eq!(y1, 1.);
        // A corner has two arms; a cross four; heavy is thicker; double is two lines.
        assert_eq!(shapes('┌', 8., 16., 1.).unwrap().len(), 2);
        assert_eq!(shapes('┼', 8., 16., 1.).unwrap().len(), 4);
        assert!(shapes('━', 8., 16., 1.).unwrap()[0].h * 16. >= 2.);
        assert_eq!(shapes('═', 8., 16., 1.).unwrap().len(), 4);
        assert_eq!(
            shapes('█', 8., 16., 1.).unwrap(),
            [Rect {
                x: 0.,
                y: 0.,
                w: 1.,
                h: 1.,
                alpha: 1.
            }]
        );
        assert_eq!(shapes('▒', 8., 16., 1.).unwrap()[0].alpha, 0.5);
        assert_eq!(shapes('▄', 8., 16., 1.).unwrap()[0].y, 0.5);
        assert_eq!(shapes('▁', 8., 16., 1.).unwrap()[0].h, 1. / 8.);
        assert_eq!(shapes('▏', 8., 16., 1.).unwrap()[0].w, 1. / 8.);
        assert_eq!(shapes('▉', 8., 16., 1.).unwrap()[0].w, 7. / 8.);
        assert!(shapes('a', 8., 16., 1.).is_none());
        assert!(is_drawn('╬') && !is_drawn('x'));
    }
}
