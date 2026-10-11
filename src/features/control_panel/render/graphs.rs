//! The status picture's graphs: the small line graphs on the tiles and feature cards, the
//! error bars, and scaling samples to positions.

use super::history::History;
use crate::util::svg::{LINE, Svg};

/// The samples of `key` since `since`, with the current value added at the end.
pub(super) fn series(
    history: &History,
    key: &str,
    since: i64,
    now: Option<(i64, f64)>,
) -> Vec<(i64, f64)> {
    let mut points: Vec<(i64, f64)> = history
        .get(key)
        .map(|samples| {
            samples
                .iter()
                .filter(|(at, _)| *at >= since)
                .copied()
                .collect()
        })
        .unwrap_or_default();
    points.extend(now);
    points
}

/// A small line graph of `points` over the times `span`, filled underneath. Nothing with
/// fewer than two points.
pub(super) fn sparkline(
    svg: &mut Svg,
    area: [f32; 4],
    points: &[(i64, f64)],
    span: (i64, i64),
    color: &str,
) {
    if points.len() < 2 {
        return;
    }
    let low = points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let high = points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    let coords = scale(points, area, span, (low, high));
    let line = path(&coords);
    let [_, y, _, h] = area;
    let (first, last) = (coords[0].0, coords[coords.len() - 1].0);
    svg.raw(format!(
        "<path d=\"{line} L{last:.1},{:.1} L{first:.1},{:.1} Z\" fill=\"{color}\" fill-opacity=\"0.15\"/>",
        y + h,
        y + h
    ));
    svg.raw(format!(
        "<path d=\"{line}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"1.5\" stroke-linejoin=\"round\" stroke-linecap=\"round\"/>"
    ));
}

/// Bars for `counts`, left to right, scaled to the highest. Empty slots show as a dot.
pub(super) fn bars(svg: &mut Svg, [x, y, w, h]: [f32; 4], counts: &[usize], color: &str) {
    let most = counts.iter().copied().max().unwrap_or(0).max(1) as f32;
    let slot = w / counts.len() as f32;
    for (n, &count) in counts.iter().enumerate() {
        let bar_x = x + slot * n as f32 + 0.5;
        if count == 0 {
            svg.rect([bar_x, y + h - 2.0, slot - 1.0, 2.0], LINE, 1.0, "");
        } else {
            let bar_h = (h * count as f32 / most).max(3.0);
            svg.rect([bar_x, y + h - bar_h, slot - 1.0, bar_h], color, 1.0, "");
        }
    }
}

/// Points in time and value to positions in `area`. A flat line sits in the middle.
pub(super) fn scale(
    points: &[(i64, f64)],
    [x, y, w, h]: [f32; 4],
    (from, to): (i64, i64),
    (low, high): (f64, f64),
) -> Vec<(f32, f32)> {
    let length = (to - from).max(1) as f32;
    points
        .iter()
        .map(|&(at, value)| {
            let px = x + w * ((at - from) as f32 / length).clamp(0.0, 1.0);
            let share = if high - low < 1e-9 {
                0.5
            } else {
                ((value - low) / (high - low)) as f32
            };
            (px, y + h - h * share.clamp(0.0, 1.0))
        })
        .collect()
}

/// "M1,2 L3,4 ..."
pub(super) fn path(coords: &[(f32, f32)]) -> String {
    coords
        .iter()
        .enumerate()
        .map(|(n, (x, y))| format!("{}{x:.1},{y:.1}", if n == 0 { "M" } else { " L" }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_scaling() {
        assert_eq!(path(&[(1.0, 2.0), (3.0, 4.0)]), "M1.0,2.0 L3.0,4.0");
        // A flat line sits in the middle.
        assert_eq!(
            scale(&[(5, 1.0)], [0.0, 0.0, 10.0, 10.0], (0, 10), (1.0, 1.0)),
            [(5.0, 5.0)]
        );
    }
}
