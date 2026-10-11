//! Pointer regions (`COMPOSITOR.pointer.defineRegion`): rectangles placed
//! relative to an output that report the pointer entering and leaving them.
//!
//! Hit testing runs here on every pointer motion; the config runtime only
//! hears about transitions, so a hot corner costs no runtime round trip while
//! the pointer moves elsewhere.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use smithay::utils::{Logical, Point, Rectangle};

use crate::ssd::{PointerModifierStateSnapshot, PointerMovePointSnapshot};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PointerRegionAnchor {
    #[default]
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl PointerRegionAnchor {
    /// Fraction of the output (and of the region) the anchor sits at.
    pub(crate) fn fraction(self) -> (f64, f64) {
        match self {
            Self::TopLeft => (0.0, 0.0),
            Self::Top => (0.5, 0.0),
            Self::TopRight => (1.0, 0.0),
            Self::Left => (0.0, 0.5),
            Self::Center => (0.5, 0.5),
            Self::Right => (1.0, 0.5),
            Self::BottomLeft => (0.0, 1.0),
            Self::Bottom => (0.5, 1.0),
            Self::BottomRight => (1.0, 1.0),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
pub struct PointerRegionOffset {
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct PointerRegionSize {
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PointerRegionSpec {
    pub id: String,
    /// Restrict the region to one output; every output gets its own copy otherwise.
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub anchor: PointerRegionAnchor,
    /// Screen axes (x right, y down), applied after anchoring.
    #[serde(default)]
    pub offset: PointerRegionOffset,
    pub size: PointerRegionSize,
    /// Disable the region on an output where an edge it touches continues
    /// into another output: the pointer passes through such an edge.
    #[serde(default)]
    pub outer_edges_only: bool,
    /// Once inside, the region grows by this much before the pointer counts as leaving.
    #[serde(default)]
    pub leave_margin: f64,
    /// Report `pressure` once the pointer, inside the region, has been pushed
    /// this far (logical px) against the edge of the output layout.
    #[serde(default)]
    pub pressure: Option<f64>,
    #[serde(default)]
    pub ignore_when_fullscreen: bool,
    #[serde(default)]
    pub ignore_while_button_held: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PointerRegionPhase {
    Enter,
    Leave,
    Pressure,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PointerRegionEventSnapshot {
    pub id: String,
    pub phase: PointerRegionPhase,
    pub output_name: String,
    pub position: PointerMovePointSnapshot,
    pub modifiers: PointerModifierStateSnapshot,
    pub timestamp: u64,
}

/// An output as the region resolver sees it.
#[derive(Debug, Clone)]
pub struct PointerRegionOutput {
    pub name: String,
    pub geometry: Rectangle<i32, Logical>,
    pub fullscreen: bool,
}

/// One pointer motion, after the compositor clamped it to the output layout.
#[derive(Debug, Clone, Copy)]
pub struct PointerRegionMotion {
    pub position: Point<f64, Logical>,
    /// The part of the motion the layout clamp discarded.
    pub push: Point<f64, Logical>,
    pub button_held: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PointerRegionTransition {
    pub id: String,
    pub phase: PointerRegionPhase,
    pub output_name: String,
}

#[derive(Debug, Clone)]
struct Inside {
    output: String,
    pressure: f64,
    pressure_reported: bool,
}

#[derive(Debug, Default)]
pub struct PointerRegionTracker {
    regions: Vec<PointerRegionSpec>,
    inside: HashMap<String, Inside>,
}

impl PointerRegionTracker {
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// Replace the region set. A region that stays keeps its inside state,
    /// so re-registering the same id does not report a fresh `enter`; a
    /// removed region is dropped without a `leave`.
    pub fn set_regions(&mut self, regions: Vec<PointerRegionSpec>) {
        let mut unique: Vec<PointerRegionSpec> = Vec::with_capacity(regions.len());
        for region in regions {
            if let Some(existing) = unique.iter_mut().find(|r| r.id == region.id) {
                *existing = region;
            } else {
                unique.push(region);
            }
        }
        self.inside
            .retain(|id, _| unique.iter().any(|region| &region.id == id));
        self.regions = unique;
    }

    /// Feed one pointer motion; returns the transitions in report order:
    /// every `leave` first, then `enter`, then `pressure`.
    pub fn update(
        &mut self,
        outputs: &[PointerRegionOutput],
        motion: PointerRegionMotion,
    ) -> Vec<PointerRegionTransition> {
        let mut leaves = Vec::new();
        let mut enters = Vec::new();
        let mut pressures = Vec::new();
        for region in &self.regions {
            let current = self.inside.get(&region.id);
            let margin = if current.is_some() {
                region.leave_margin.max(0.0)
            } else {
                0.0
            };
            let hit = if region.ignore_while_button_held && motion.button_held {
                None
            } else {
                outputs.iter().find_map(|output| {
                    if region.ignore_when_fullscreen && output.fullscreen {
                        return None;
                    }
                    let rect = region_rect(region, output, outputs, margin)?;
                    contains(rect, motion.position).then(|| output.name.clone())
                })
            };

            match (current, hit) {
                (Some(inside), Some(output)) if inside.output == output => {}
                (Some(inside), hit) => {
                    leaves.push(PointerRegionTransition {
                        id: region.id.clone(),
                        phase: PointerRegionPhase::Leave,
                        output_name: inside.output.clone(),
                    });
                    if let Some(output) = hit {
                        enters.push((region, output));
                    }
                }
                (None, Some(output)) => enters.push((region, output)),
                (None, None) => {}
            }
        }

        for leave in &leaves {
            self.inside.remove(&leave.id);
        }
        let mut result = leaves;
        for (region, output) in enters {
            self.inside.insert(
                region.id.clone(),
                Inside {
                    output: output.clone(),
                    pressure: 0.0,
                    pressure_reported: false,
                },
            );
            result.push(PointerRegionTransition {
                id: region.id.clone(),
                phase: PointerRegionPhase::Enter,
                output_name: output,
            });
        }

        let push = motion.push.x.abs() + motion.push.y.abs();
        if push > 0.0 {
            for region in &self.regions {
                let Some(threshold) = region.pressure else {
                    continue;
                };
                let Some(inside) = self.inside.get_mut(&region.id) else {
                    continue;
                };
                if inside.pressure_reported {
                    continue;
                }
                inside.pressure += push;
                if inside.pressure >= threshold {
                    inside.pressure_reported = true;
                    pressures.push(PointerRegionTransition {
                        id: region.id.clone(),
                        phase: PointerRegionPhase::Pressure,
                        output_name: inside.output.clone(),
                    });
                }
            }
        }
        result.extend(pressures);
        result
    }
}

fn contains(rect: Rectangle<f64, Logical>, point: Point<f64, Logical>) -> bool {
    point.x >= rect.loc.x
        && point.y >= rect.loc.y
        && point.x < rect.loc.x + rect.size.w
        && point.y < rect.loc.y + rect.size.h
}

/// The region's rectangle on `output`, clipped to it, or `None` when the
/// region does not apply there.
fn region_rect(
    region: &PointerRegionSpec,
    output: &PointerRegionOutput,
    outputs: &[PointerRegionOutput],
    margin: f64,
) -> Option<Rectangle<f64, Logical>> {
    if region
        .output
        .as_ref()
        .is_some_and(|name| name != &output.name)
    {
        return None;
    }
    let width = region.size.width;
    let height = region.size.height;
    if !(width > 0.0 && height > 0.0) {
        return None;
    }
    let geometry = output.geometry.to_f64();
    let (fx, fy) = region.anchor.fraction();
    let x = geometry.loc.x + fx * (geometry.size.w - width) + region.offset.x;
    let y = geometry.loc.y + fy * (geometry.size.h - height) + region.offset.y;

    let left = x.max(geometry.loc.x);
    let top = y.max(geometry.loc.y);
    let right = (x + width).min(geometry.loc.x + geometry.size.w);
    let bottom = (y + height).min(geometry.loc.y + geometry.size.h);
    if right <= left || bottom <= top {
        return None;
    }

    if region.outer_edges_only && touches_inner_edge(output, outputs, left, top, right, bottom) {
        return None;
    }

    let left = (left - margin).max(geometry.loc.x);
    let top = (top - margin).max(geometry.loc.y);
    let right = (right + margin).min(geometry.loc.x + geometry.size.w);
    let bottom = (bottom + margin).min(geometry.loc.y + geometry.size.h);
    Some(Rectangle::new(
        (left, top).into(),
        (right - left, bottom - top).into(),
    ))
}

/// Whether the clipped rect touches an output edge another output continues.
fn touches_inner_edge(
    output: &PointerRegionOutput,
    outputs: &[PointerRegionOutput],
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
) -> bool {
    let geometry = output.geometry.to_f64();
    let output_right = geometry.loc.x + geometry.size.w;
    let output_bottom = geometry.loc.y + geometry.size.h;
    let overlaps = |a0: f64, a1: f64, b0: f64, b1: f64| a0 < b1 && b0 < a1;
    outputs
        .iter()
        .filter(|other| other.name != output.name)
        .any(|other| {
            let other = other.geometry.to_f64();
            let other_right = other.loc.x + other.size.w;
            let other_bottom = other.loc.y + other.size.h;
            let vertical = overlaps(top, bottom, other.loc.y, other_bottom);
            let horizontal = overlaps(left, right, other.loc.x, other_right);
            (left <= geometry.loc.x && other_right == geometry.loc.x && vertical)
                || (right >= output_right && other.loc.x == output_right && vertical)
                || (top <= geometry.loc.y && other_bottom == geometry.loc.y && horizontal)
                || (bottom >= output_bottom && other.loc.y == output_bottom && horizontal)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(name: &str, x: i32, y: i32, w: i32, h: i32) -> PointerRegionOutput {
        PointerRegionOutput {
            name: name.into(),
            geometry: Rectangle::new((x, y).into(), (w, h).into()),
            fullscreen: false,
        }
    }

    fn corner(id: &str, anchor: PointerRegionAnchor) -> PointerRegionSpec {
        PointerRegionSpec {
            id: id.into(),
            output: None,
            anchor,
            offset: PointerRegionOffset::default(),
            size: PointerRegionSize {
                width: 2.0,
                height: 2.0,
            },
            outer_edges_only: true,
            leave_margin: 0.0,
            pressure: None,
            ignore_when_fullscreen: false,
            ignore_while_button_held: false,
        }
    }

    fn at(x: f64, y: f64) -> PointerRegionMotion {
        PointerRegionMotion {
            position: (x, y).into(),
            push: (0.0, 0.0).into(),
            button_held: false,
        }
    }

    fn phases(transitions: &[PointerRegionTransition]) -> Vec<(&str, PointerRegionPhase, &str)> {
        transitions
            .iter()
            .map(|t| (t.id.as_str(), t.phase, t.output_name.as_str()))
            .collect()
    }

    #[test]
    fn region_follows_its_anchor_and_reports_enter_and_leave_once() {
        let outputs = [output("A", 0, 0, 100, 50)];
        let mut tracker = PointerRegionTracker::default();
        tracker.set_regions(vec![
            corner("tl", PointerRegionAnchor::TopLeft),
            corner("br", PointerRegionAnchor::BottomRight),
        ]);
        assert!(tracker.update(&outputs, at(50.0, 25.0)).is_empty());
        assert_eq!(
            phases(&tracker.update(&outputs, at(0.0, 1.5))),
            [("tl", PointerRegionPhase::Enter, "A")]
        );
        assert!(tracker.update(&outputs, at(1.0, 0.0)).is_empty());
        assert_eq!(
            phases(&tracker.update(&outputs, at(99.5, 49.9))),
            [
                ("tl", PointerRegionPhase::Leave, "A"),
                ("br", PointerRegionPhase::Enter, "A"),
            ]
        );
        assert_eq!(
            phases(&tracker.update(&outputs, at(97.9, 49.9))),
            [("br", PointerRegionPhase::Leave, "A")]
        );
    }

    #[test]
    fn edge_anchor_centres_the_region_and_offset_moves_it() {
        let outputs = [output("A", 100, 0, 200, 100)];
        let mut region = corner("top", PointerRegionAnchor::Top);
        region.size = PointerRegionSize {
            width: 20.0,
            height: 1.0,
        };
        region.offset = PointerRegionOffset { x: 10.0, y: 0.0 };
        let mut tracker = PointerRegionTracker::default();
        tracker.set_regions(vec![region]);
        // 100 + (200 - 20) / 2 + 10 = 200 .. 220
        assert!(tracker.update(&outputs, at(199.0, 0.0)).is_empty());
        assert_eq!(tracker.update(&outputs, at(200.0, 0.0)).len(), 1);
        assert_eq!(tracker.update(&outputs, at(220.0, 0.0)).len(), 1);
    }

    #[test]
    fn every_output_has_a_copy_unless_one_is_named() {
        let outputs = [output("A", 0, 0, 100, 100), output("B", 0, 200, 100, 100)];
        let mut tracker = PointerRegionTracker::default();
        tracker.set_regions(vec![corner("tl", PointerRegionAnchor::TopLeft)]);
        assert_eq!(
            phases(&tracker.update(&outputs, at(0.0, 200.0))),
            [("tl", PointerRegionPhase::Enter, "B")]
        );
        // Moving straight to the same corner of another output is a leave then an enter.
        assert_eq!(
            phases(&tracker.update(&outputs, at(0.0, 0.0))),
            [
                ("tl", PointerRegionPhase::Leave, "B"),
                ("tl", PointerRegionPhase::Enter, "A"),
            ]
        );

        let mut named = corner("tl", PointerRegionAnchor::TopLeft);
        named.output = Some("A".into());
        tracker.set_regions(vec![named]);
        tracker.update(&outputs, at(50.0, 50.0));
        assert!(tracker.update(&outputs, at(0.0, 200.0)).is_empty());
    }

    #[test]
    fn outer_edges_only_skips_corners_another_output_continues() {
        // B sits to the right of A, top-aligned: A's top-right and B's top-left
        // corners are a shared edge the pointer simply crosses.
        let outputs = [output("A", 0, 0, 100, 100), output("B", 100, 0, 100, 100)];
        let mut tracker = PointerRegionTracker::default();
        tracker.set_regions(vec![
            corner("tr", PointerRegionAnchor::TopRight),
            corner("tl", PointerRegionAnchor::TopLeft),
        ]);
        assert!(tracker.update(&outputs, at(99.0, 0.0)).is_empty());
        assert!(tracker.update(&outputs, at(100.0, 0.0)).is_empty());
        assert_eq!(
            phases(&tracker.update(&outputs, at(0.0, 0.0))),
            [("tl", PointerRegionPhase::Enter, "A")]
        );
        assert_eq!(
            phases(&tracker.update(&outputs, at(199.0, 0.0))),
            [
                ("tl", PointerRegionPhase::Leave, "A"),
                ("tr", PointerRegionPhase::Enter, "B"),
            ]
        );

        // An output offset below the corner leaves that corner reachable.
        let outputs = [output("A", 0, 0, 100, 100), output("B", 100, 50, 100, 100)];
        tracker.set_regions(vec![corner("tr", PointerRegionAnchor::TopRight)]);
        tracker.update(&outputs, at(50.0, 50.0));
        assert_eq!(
            phases(&tracker.update(&outputs, at(99.0, 0.0))),
            [("tr", PointerRegionPhase::Enter, "A")]
        );

        let mut everywhere = corner("tr", PointerRegionAnchor::TopRight);
        everywhere.outer_edges_only = false;
        let outputs = [output("A", 0, 0, 100, 100), output("B", 100, 0, 100, 100)];
        tracker.set_regions(vec![everywhere]);
        tracker.update(&outputs, at(50.0, 50.0));
        assert_eq!(tracker.update(&outputs, at(99.0, 0.0)).len(), 1);
    }

    #[test]
    fn leave_margin_keeps_the_pointer_inside_until_it_moves_away() {
        let outputs = [output("A", 0, 0, 100, 100)];
        let mut region = corner("tl", PointerRegionAnchor::TopLeft);
        region.leave_margin = 8.0;
        let mut tracker = PointerRegionTracker::default();
        tracker.set_regions(vec![region]);
        // Not entered through the margin.
        assert!(tracker.update(&outputs, at(5.0, 5.0)).is_empty());
        assert_eq!(tracker.update(&outputs, at(1.0, 1.0)).len(), 1);
        assert!(tracker.update(&outputs, at(9.0, 9.0)).is_empty());
        assert_eq!(
            phases(&tracker.update(&outputs, at(10.0, 0.0))),
            [("tl", PointerRegionPhase::Leave, "A")]
        );
    }

    #[test]
    fn pressure_accumulates_while_inside_and_reports_once_per_visit() {
        let outputs = [output("A", 0, 0, 100, 100)];
        let mut region = corner("tl", PointerRegionAnchor::TopLeft);
        region.pressure = Some(10.0);
        let mut tracker = PointerRegionTracker::default();
        tracker.set_regions(vec![region]);
        let push = |x, y| PointerRegionMotion {
            position: (0.0, 0.0).into(),
            push: (x, y).into(),
            button_held: false,
        };
        // Pushing outside the region does not count.
        tracker.update(&outputs, at(50.0, 0.0));
        assert!(
            tracker
                .update(&outputs, PointerRegionMotion {
                    position: (50.0, 0.0).into(),
                    push: (0.0, -30.0).into(),
                    button_held: false,
                })
                .is_empty()
        );
        assert_eq!(
            phases(&tracker.update(&outputs, push(-4.0, -2.0))),
            [("tl", PointerRegionPhase::Enter, "A")]
        );
        assert!(tracker.update(&outputs, push(-3.0, 0.0)).is_empty());
        assert_eq!(
            phases(&tracker.update(&outputs, push(-1.0, 0.0))),
            [("tl", PointerRegionPhase::Pressure, "A")]
        );
        assert!(tracker.update(&outputs, push(-50.0, 0.0)).is_empty());
        tracker.update(&outputs, at(50.0, 50.0));
        tracker.update(&outputs, at(0.0, 0.0));
        assert_eq!(
            phases(&tracker.update(&outputs, push(-10.0, 0.0))),
            [("tl", PointerRegionPhase::Pressure, "A")]
        );
    }

    #[test]
    fn fullscreen_and_held_buttons_disable_the_region() {
        let mut outputs = [output("A", 0, 0, 100, 100)];
        let mut region = corner("tl", PointerRegionAnchor::TopLeft);
        region.ignore_when_fullscreen = true;
        region.ignore_while_button_held = true;
        let mut tracker = PointerRegionTracker::default();
        tracker.set_regions(vec![region]);
        let held = PointerRegionMotion {
            button_held: true,
            ..at(0.0, 0.0)
        };
        assert!(tracker.update(&outputs, held).is_empty());
        assert_eq!(tracker.update(&outputs, at(0.0, 0.0)).len(), 1);
        assert_eq!(
            phases(&tracker.update(&outputs, held)),
            [("tl", PointerRegionPhase::Leave, "A")]
        );
        outputs[0].fullscreen = true;
        assert!(tracker.update(&outputs, at(0.0, 0.0)).is_empty());
    }

    #[test]
    fn re_registering_keeps_state_and_removal_is_silent() {
        let outputs = [output("A", 0, 0, 100, 100)];
        let mut tracker = PointerRegionTracker::default();
        tracker.set_regions(vec![corner("tl", PointerRegionAnchor::TopLeft)]);
        assert_eq!(tracker.update(&outputs, at(0.0, 0.0)).len(), 1);
        tracker.set_regions(vec![corner("tl", PointerRegionAnchor::TopLeft)]);
        assert!(tracker.update(&outputs, at(1.0, 0.0)).is_empty());
        tracker.set_regions(Vec::new());
        assert!(tracker.update(&outputs, at(50.0, 50.0)).is_empty());
        assert!(tracker.is_empty());
    }

    #[test]
    fn spec_parses_from_the_runtime_json() {
        let spec: PointerRegionSpec = serde_json::from_str(
            r#"{"id":"hot","anchor":"bottom-right","size":{"width":1,"height":1},
                "outerEdgesOnly":true,"leaveMargin":4,"pressure":100}"#,
        )
        .unwrap();
        assert_eq!(spec.anchor, PointerRegionAnchor::BottomRight);
        assert!(spec.outer_edges_only);
        assert_eq!(spec.leave_margin, 4.0);
        assert_eq!(spec.pressure, Some(100.0));
        assert_eq!(spec.offset, PointerRegionOffset::default());
    }
}
