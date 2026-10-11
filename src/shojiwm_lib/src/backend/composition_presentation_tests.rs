//! Presentation bookkeeping for offscreen composition sources: a client drawn
//! through a render texture, a Scene3D plane or a window snapshot stays
//! eligible for frame callbacks only while that path is visible on the output.
#![cfg(test)]

use std::collections::{HashMap, HashSet};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{
    Id, Kind, PrimaryScanoutOutput, RenderElementPresentationState, RenderElementState,
    RenderElementStates,
};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Bind, Offscreen};
use smithay::utils::{Rectangle, Scale, Transform};

use super::composition::*;

fn rendered(area: usize) -> RenderElementState {
    RenderElementState {
        visible_area: area,
        presentation_state: RenderElementPresentationState::Rendering { reason: None },
        needs_capture: false,
    }
}

fn states(id: &Id, state: RenderElementState) -> RenderElementStates {
    RenderElementStates {
        states: HashMap::from([(id.clone(), state)]),
    }
}

#[test]
fn nested_sources_are_presented_only_through_visible_parents() {
    let (scene, texture, source, hidden) = (Id::new(), Id::new(), Id::new(), Id::new());
    let skipped = RenderElementState {
        visible_area: 0,
        presentation_state: RenderElementPresentationState::Skipped,
        needs_capture: false,
    };
    let mut input = states(&source, rendered(50));
    input.states.insert(hidden.clone(), skipped);
    let presentation = CompositionPresentation {
        snapshots: HashSet::new(),
        content: HashMap::from([
            (scene.clone(), (400, states(&texture, rendered(100)))),
            (texture.clone(), (100, input)),
        ]),
    };
    let visible = presentation.presented_states(&states(&scene, rendered(200)));
    assert_eq!(
        visible
            .element_render_state(source.clone())
            .unwrap()
            .visible_area,
        25
    );
    assert!(!visible.element_was_presented(hidden));
    assert!(
        presentation
            .presented_states(&states(&scene, skipped))
            .states
            .is_empty()
    );
    assert!(
        presentation
            .presented_states(&RenderElementStates::default())
            .states
            .is_empty()
    );
    let mut primary = PrimaryScanoutOutput::default();
    let output = smithay::output::Output::new(
        "preview".into(),
        smithay::output::PhysicalProperties {
            size: (0, 0).into(),
            subpixel: smithay::output::Subpixel::Unknown,
            make: "test".into(),
            model: "test".into(),
            serial_number: String::new(),
        },
    );
    assert_eq!(
        primary.update_from_render_element_states(
            source.clone(),
            &output,
            None,
            &visible,
            crate::presentation::area_primary_scanout_compare
        ),
        Some(output.clone())
    );
    assert_eq!(
        primary.update_from_render_element_states(
            source,
            &output,
            None,
            &RenderElementStates::default(),
            crate::presentation::area_primary_scanout_compare
        ),
        None
    );
}

#[test]
fn visible_composition_survives_a_skipped_direct_copy_without_claiming_scanout() {
    let source = Id::new();
    let mut direct = states(
        &source,
        RenderElementState {
            visible_area: 0,
            presentation_state: RenderElementPresentationState::Skipped,
            needs_capture: false,
        },
    );
    merge_presented_states(&mut direct, &states(&source, rendered(20)));
    assert_eq!(
        direct
            .element_render_state(source.clone())
            .unwrap()
            .visible_area,
        20
    );
    assert_eq!(
        direct
            .element_render_state(source.clone())
            .unwrap()
            .presentation_state,
        RenderElementPresentationState::Rendering { reason: None }
    );
    merge_presented_states(&mut direct, &states(&source, rendered(10)));
    assert_eq!(
        direct.element_render_state(source).unwrap().visible_area,
        20
    );
}

#[test]
fn native_snapshots_keep_native_output_selection_until_used_by_a_composition() {
    let (snapshot, source, scene) = (Id::new(), Id::new(), Id::new());
    let presentation = CompositionPresentation {
        content: HashMap::from([
            (snapshot.clone(), (100, states(&source, rendered(100)))),
            (scene.clone(), (100, states(&snapshot, rendered(100)))),
        ]),
        snapshots: HashSet::from([snapshot.clone()]),
    };
    let native = states(&snapshot, rendered(50));
    assert!(
        presentation
            .source_states(&native)
            .element_was_presented(source.clone())
    );
    assert!(presentation.presented_states(&native).states.is_empty());
    assert!(
        presentation
            .presented_states(&states(&scene, rendered(50)))
            .element_was_presented(source)
    );
}

struct GpuScene {
    renderer: GlesRenderer,
    source: Id,
    source_commit: CommitCounter,
    color: [f32; 4],
    snapshot: Option<super::snapshot::LiveWindowSnapshot>,
    use_snapshot: bool,
}

impl CompositionScene for GpuScene {
    type Element = crate::backend::tty::TtyRenderElements;
    fn renderer(&mut self) -> &mut GlesRenderer {
        &mut self.renderer
    }
    fn layers(
        &mut self,
        _: &SceneContext,
        _: &[LayerKind],
        _: &[BelowNode<'_>],
    ) -> Result<Vec<Self::Element>, Box<dyn std::error::Error>> {
        Ok(vec![])
    }
    fn layer_popups(
        &mut self,
        _: &SceneContext,
        _: bool,
    ) -> Result<Vec<Self::Element>, Box<dyn std::error::Error>> {
        Ok(vec![])
    }
    fn windows(
        &mut self,
        _: &SceneContext,
        _: WindowSelection<'_>,
        _: &[BelowNode<'_>],
    ) -> Result<Vec<Self::Element>, Box<dyn std::error::Error>> {
        let elements = vec![Self::Element::Blink(SolidColorRenderElement::new(
            self.source.clone(),
            Rectangle::from_size((16, 16).into()),
            self.source_commit,
            self.color,
            Kind::Unspecified,
        ))];
        if self.use_snapshot {
            let mut tracker = OutputDamageTracker::new((16, 16), 1.0, Transform::Normal);
            self.snapshot = super::snapshot::capture_snapshot(
                &mut self.renderer,
                self.snapshot.take(),
                &mut tracker,
                crate::ssd::LogicalRect::new(0, 0, 16, 16),
                0,
                true,
                Scale::from(1.0),
                &elements,
            )
            .unwrap();
            let snapshot = self.snapshot.as_ref().unwrap();
            Ok(vec![Self::Element::Snapshot(texture_element(
                &self.renderer,
                &snapshot.id,
                &snapshot.texture,
                &snapshot.damage,
                Rectangle::from_size((16, 16).into()),
                Scale::from(1.0),
                1.0,
            ))])
        } else {
            Ok(elements)
        }
    }
    fn texture(&self, e: TextureRenderElement<GlesTexture>) -> Self::Element {
        Self::Element::Snapshot(e)
    }
    fn solid(&self, e: SolidColorRenderElement) -> Self::Element {
        Self::Element::Blink(e)
    }
}

#[test]
#[ignore = "requires surfaceless EGL; renders only offscreen buffers"]
fn composition_gpu_preserves_live_source_visibility_across_cached_and_hidden_frames() {
    use smithay::backend::egl::{EGLContext, EGLDisplay, native::EGLSurfacelessDisplay};
    let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }.unwrap();
    let context = EGLContext::new(&display).unwrap();
    let renderer = unsafe { GlesRenderer::new(context) }.unwrap();
    let mut scene = GpuScene {
        renderer,
        source: Id::new(),
        source_commit: CommitCounter::default(),
        color: [1.0, 0.0, 0.0, 1.0],
        snapshot: None,
        use_snapshot: false,
    };
    let mut plan: OutputComposition = serde_json::from_str(
        r#"{
      "nodes": [{"kind":"scene3d", "projection":[1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1],
        "view":[1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1], "antialias":false,
        "objects":[{"kind":"plane","texture":1,"width":2,"height":2}]}],
      "textures":[{"key":"source", "width":16,"height":16,"nodes":[{"kind":"windows"}]},
        {"key":"nested","width":16,"height":16,"nodes":[{"kind":"texture-view","texture":0}]}]
    }"#,
    )
    .unwrap();
    let ctx = SceneContext {
        output_geo: Rectangle::from_size((16, 16).into()),
        scale: Scale::from(1.0),
        primary: true,
        scope: "test".into(),
    };
    let mut targets = CompositionTargets::default();
    let mut tracker = OutputDamageTracker::new((16, 16), 1.0, Transform::Normal);
    let mut buffer: GlesTexture =
        Offscreen::create_buffer(&mut scene.renderer, Fourcc::Abgr8888, (16, 16).into()).unwrap();
    let draw = |scene: &mut GpuScene,
                targets: &mut CompositionTargets,
                tracker: &mut OutputDamageTracker,
                buffer: &mut GlesTexture,
                plan: &OutputComposition,
                age| {
        let mut built = Builder::new(plan, "test")
            .build(scene, targets, &ctx)
            .unwrap();
        if let Some(snapshot) = &scene.snapshot {
            built.presentation.add_snapshot(snapshot);
        }
        let mut framebuffer = scene.renderer.bind(buffer).unwrap();
        let result = tracker
            .render_output(
                &mut scene.renderer,
                &mut framebuffer,
                age,
                &built.elements,
                [0.0; 4],
            )
            .unwrap();
        (
            built.presentation.presented_states(&result.states),
            result.damage.is_some(),
        )
    };
    let (shown, damaged) = draw(
        &mut scene,
        &mut targets,
        &mut tracker,
        &mut buffer,
        &plan,
        0,
    );
    assert!(shown.element_was_presented(scene.source.clone()));
    assert!(damaged);
    let generation = targets.textures["source"].generation;
    let (shown, damaged) = draw(
        &mut scene,
        &mut targets,
        &mut tracker,
        &mut buffer,
        &plan,
        1,
    );
    assert!(
        shown.element_was_presented(scene.source.clone()),
        "idle cache must retain callback eligibility"
    );
    assert!(!damaged);
    assert_eq!(targets.textures["source"].generation, generation);
    scene.color = [0.0, 1.0, 0.0, 1.0];
    scene.source_commit.increment();
    let (shown, damaged) = draw(
        &mut scene,
        &mut targets,
        &mut tracker,
        &mut buffer,
        &plan,
        1,
    );
    assert!(shown.element_was_presented(scene.source.clone()));
    assert!(damaged);
    assert!(targets.textures["source"].generation > generation);
    scene.use_snapshot = true;
    assert!(
        draw(
            &mut scene,
            &mut targets,
            &mut tracker,
            &mut buffer,
            &plan,
            1
        )
        .0
        .element_was_presented(scene.source.clone()),
        "a transformed window inside the preview must retain its source"
    );
    scene.use_snapshot = false;
    plan.nodes.push(CompositionNode::Solid {
        rect: None,
        color: [0.0, 0.0, 0.0, 1.0],
    });
    assert!(
        !draw(
            &mut scene,
            &mut targets,
            &mut tracker,
            &mut buffer,
            &plan,
            1
        )
        .0
        .element_was_presented(scene.source.clone()),
        "covered scene must not keep clients active"
    );
    plan.nodes.pop();
    plan.textures[0].nodes.push(CompositionNode::Solid {
        rect: None,
        color: [0.0, 0.0, 0.0, 1.0],
    });
    assert!(
        !draw(
            &mut scene,
            &mut targets,
            &mut tracker,
            &mut buffer,
            &plan,
            1
        )
        .0
        .element_was_presented(scene.source.clone()),
        "covered input must not keep clients active"
    );
    plan.textures[0].nodes.pop();
    if let CompositionNode::Scene3d(spec) = &mut plan.nodes[0] {
        let Object3d::Plane { opacity, .. } = &mut spec.objects[0];
        *opacity = 0.0;
    }
    assert!(
        !draw(
            &mut scene,
            &mut targets,
            &mut tracker,
            &mut buffer,
            &plan,
            1
        )
        .0
        .element_was_presented(scene.source.clone()),
        "transparent plane must not keep clients active"
    );
    plan.nodes.clear();
    assert!(
        draw(
            &mut scene,
            &mut targets,
            &mut tracker,
            &mut buffer,
            &plan,
            1
        )
        .0
        .states
        .is_empty()
    );
    assert!(
        targets.textures.is_empty(),
        "removed composition must release source dependencies"
    );
}
