//! Port of `packages/config/src/drag-and-drop.tsx`: what is drawn while data
//! is dragged between windows (`COMPOSITOR.dnd`).
//!
//! The application's drag image sits on a small frosted card (what is behind
//! it, blurred and tinted) with a badge for the action a drop would take
//! (+ copy, → move, ? ask, × refused), and a pill below names what is
//! dragged (text, file, image, link, data) with an icon. The card pops in
//! when the drag starts. An accepted drop sinks into the target; a drop
//! nobody accepts flies back to where the drag started, with the easing
//! every window movement uses, and fades out there (like macOS).

use shojiwm_rs::prelude::*;

use crate::window_manager::WINDOW_MANAGEMENT_EASING;

/// Pop-in when the drag starts, in ms.
const APPEAR_MS: f64 = 220.0;
/// Flying back after a drop nobody accepted, in ms.
const RETURN_MS: f64 = 420.0;
/// Sinking into the target of an accepted drop, in ms.
const DROP_MS: f64 = 180.0;
/// How much of the flight back is spent fading out, at its end.
const RETURN_FADE: f64 = 0.3;

const DROP_EASING: Easing = cubic_bezier(0.4, 0.0, 1.0, 1.0);

const FONT: [&str; 2] = ["Noto Sans CJK JP", "Noto Color Emoji"];
/// Over the blur, so text stays readable on any background.
const CARD_TINT: &str = "#1f243099";
const CARD_BORDER: &str = "#ffffff1f";
const BADGE_RING: &str = "#1f2430";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragKind {
    Text,
    File,
    Image,
    Link,
    Data,
}

impl DragKind {
    /// What the data is, from the formats it is offered in.
    fn of(mime_types: &[String]) -> Self {
        let has = |wanted: &str| mime_types.iter().any(|mime| mime == wanted);
        if has("text/x-moz-url") || has("_NETSCAPE_URL") || has("application/x-chromium-url") {
            Self::Link
        } else if has("text/uri-list") {
            Self::File
        } else if mime_types.iter().any(|mime| mime.starts_with("image/")) {
            Self::Image
        } else if mime_types
            .iter()
            .any(|mime| mime.starts_with("text/") || mime == "UTF8_STRING" || mime == "STRING")
        {
            Self::Text
        } else {
            Self::Data
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::File => "File",
            Self::Image => "Image",
            Self::Link => "Link",
            Self::Data => "Data",
        }
    }

    /// The icon drawn by `dnd-kind-icon.frag`.
    fn index(self) -> f32 {
        match self {
            Self::Text => 0.0,
            Self::File => 1.0,
            Self::Image => 2.0,
            Self::Link => 3.0,
            Self::Data => 4.0,
        }
    }

    fn color(self) -> [f32; 4] {
        match self {
            Self::Text => [0.55, 0.76, 1.0, 1.0],
            Self::File => [0.98, 0.8, 0.45, 1.0],
            Self::Image => [0.6, 0.88, 0.62, 1.0],
            Self::Link => [0.78, 0.66, 1.0, 1.0],
            Self::Data => [0.75, 0.78, 0.84, 1.0],
        }
    }
}

/// The badge on the card's corner; none while the drag is over nothing.
fn badge_for(drag: &DragSession) -> Option<(&'static str, &'static str)> {
    match drag.action().get() {
        DragAction::Copy => Some(("+", "#30d158")),
        DragAction::Move => Some(("→", "#0a84ff")),
        DragAction::Ask => Some(("?", "#ff9f0a")),
        DragAction::None => drag.target().get().is_some().then_some(("×", "#ff453a")),
    }
}

fn lerp(from: f64, to: f64, t: f64) -> f64 {
    from + (to - from) * t
}

fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// What is behind the card, blurred like the windows' backdrops.
fn card_blur() -> Effect {
    Effect::new(backdrop_source())
        .capture_padding(24)
        .invalidate(Invalidate::on_source_damage_box(8))
        .preserve_alpha()
        .stage(dual_kawase_blur(4, 2))
}

/// A blurred, tinted rounded panel laying `content` out like a `Flex`.
fn frosted_panel(radius: f64, content: Element) -> Element {
    Flex::column()
        .style(
            Style::new()
                .border_radius(radius)
                .overflow(Overflow::Hidden)
                .box_shadow(shadow(0.0, 6.0, 18.0, hex("#00000080"))),
        )
        .child(
            ShaderEffect::new(card_blur()).direction(Direction::Column).child(
                content.style(
                    Style::new()
                        .background(hex(CARD_TINT))
                        .border_radius(radius)
                        .border(1.0, hex(CARD_BORDER)),
                ),
            ),
        )
}

fn kind_icon(kind: DragKind, size: f64) -> Element {
    Flex::column()
        .paint(
            paint_shader("./src/effect/dnd-kind-icon.frag")
                .uniform("kind", kind.index())
                .uniform("color", kind.color()),
        )
        .style(Style::new().size(size, size))
}

fn drag_card(drag: &DragSession, kind: DragKind) -> Element {
    // No picture from the application: a tile with the kind's icon.
    let fallback = Flex::column()
        .style(
            Style::new()
                .size(40.0, 40.0)
                .align_items(AlignItems::Center)
                .justify_content(JustifyContent::Center),
        )
        .child(kind_icon(kind, 26.0));
    let card = frosted_panel(
        10.0,
        Flex::column()
            .style(Style::new().padding(6.0))
            .child(drag.icon_slot(fallback)),
    );
    let mut slot = Flex::column().style(Style::new().relative()).child(card);
    if let Some((text, color)) = badge_for(drag) {
        // A dark disc with the coloured one inside it, rather than a
        // coloured disc with a border: both edges of a border are
        // antialiased over the fill, which would show a coloured fringe
        // outside the ring.
        slot = slot.child(
            Flex::column()
                .style(
                    Style::new()
                        .absolute()
                        .top(-7.0)
                        .right(-7.0)
                        .size(20.0, 20.0)
                        .padding(2.0)
                        .border_radius(10.0)
                        .background(hex(BADGE_RING))
                        .box_shadow(shadow(0.0, 1.0, 4.0, hex("#00000080"))),
                )
                .child(
                    Flex::column()
                        .style(
                            Style::new()
                                .size(16.0, 16.0)
                                .border_radius(8.0)
                                .background(hex(color))
                                .align_items(AlignItems::Center)
                                .justify_content(JustifyContent::Center),
                        )
                        .child(Label::new(text).style(
                            Style::new()
                                .color(hex("#ffffff"))
                                .fonts(FONT)
                                .font_size(12.0)
                                .font_weight(700),
                        )),
                ),
        );
    }
    let pill = frosted_panel(
        11.0,
        Flex::row()
            .style(
                Style::new()
                    .padding_left(8.0)
                    .padding_right(10.0)
                    .padding_y(3.0)
                    .gap(6.0)
                    .align_items(AlignItems::Center),
            )
            .child(kind_icon(kind, 13.0))
            .child(Label::new(kind.label()).style(
                Style::new().color(hex("#e6e9ef")).fonts(FONT).font_size(12.0),
            )),
    );
    Flex::column()
        .style(Style::new().align_items(AlignItems::Center).gap(6.0))
        .child(slot)
        .child(pill)
}

pub fn install_drag_and_drop() {
    COMPOSITOR.dnd.on_start(|drag| {
        let kind = DragKind::of(drag.mime_types());
        // Created in the drag's scope: they go away with it.
        let appear = Animation::new(0.0);
        let finish = Animation::new(0.0);
        appear.start(
            AnimationOptions::to(1.0, APPEAR_MS)
                .from(0.0)
                .easing(WINDOW_MANAGEMENT_EASING),
        );

        // Until the drag ends the view rides on the pointer; then it is
        // pinned to the screen and either sinks in place (accepted) or flies
        // back to where the drag started (refused or cancelled).
        let ended = drag.ended();
        let origin = drag.origin();
        let position = move |axis: fn(DragPoint) -> f64| {
            derive(move || match ended.get() {
                None => 0.0,
                Some(end) if end.accepted => axis(end.position),
                Some(end) => lerp(axis(end.position), axis(origin), finish.get()),
            })
        };

        let view = drag.show(
            DragViewOptions::new()
                .anchor(derive(move || {
                    if ended.get().is_some() { DragViewAnchor::Global } else { DragViewAnchor::Pointer }
                }))
                .x(position(|point| point.x))
                .y(position(|point| point.y))
                // A refusing window only gets the × badge: fading the card
                // would let what is behind show through its blur.
                .opacity(derive(move || match ended.get() {
                    None => appear.get(),
                    Some(end) if end.accepted => 1.0 - finish.get(),
                    Some(_) => 1.0 - smoothstep(1.0 - RETURN_FADE, 1.0, finish.get()),
                }))
                .scale(derive(move || {
                    let popped = 0.85 + 0.15 * appear.get();
                    match ended.get() {
                        None => popped,
                        Some(end) if end.accepted => popped * (1.0 - 0.4 * finish.get()),
                        Some(_) => popped * (1.0 - 0.08 * finish.get()),
                    }
                }))
                .content({
                    let drag = drag.clone();
                    move || drag_card(&drag, kind)
                }),
        );

        drag.on_end(move |end| {
            let (duration, easing) = if end.accepted {
                (DROP_MS, DROP_EASING)
            } else {
                (RETURN_MS, WINDOW_MANAGEMENT_EASING)
            };
            finish.start_then(
                AnimationOptions::to(1.0, duration).from(0.0).easing(easing),
                move |_| view.dispose(),
            );
        });
    });
}
