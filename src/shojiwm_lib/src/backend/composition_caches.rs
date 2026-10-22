//! Per-render-target effect caches of custom compositions.
//!
//! Effect caches (captured backdrops, framebuffer effect state, window effect
//! results) hold pixels that depend on what lies behind an effect. A window
//! drawn both on its output and in a render texture sees a different backdrop
//! in each, so every target keeps its own caches. While a render texture
//! builds, its caches are swapped into the places the scene code reads them
//! from, and swapped back out when it is done. Outputs without render textures
//! never swap anything.

use std::collections::{HashMap, HashSet};

use crate::backend::shader_effect::{
    CachedBackdropTexture, ShaderEffectElementState, WindowEffectElementState,
};

/// The layer and popup caches, wherever the backend keeps them.
pub struct LayerCaches<'a> {
    pub layer_backdrop_cache: &'a mut HashMap<String, CachedBackdropTexture>,
    pub layer_framebuffer_effect_states: &'a mut HashMap<String, ShaderEffectElementState>,
    pub layer_effect_cache: &'a mut HashMap<String, WindowEffectElementState>,
    pub popup_effect_cache: &'a mut HashMap<String, WindowEffectElementState>,
    pub popup_framebuffer_effect_states: &'a mut HashMap<String, ShaderEffectElementState>,
}

#[derive(Default)]
struct StoredLayerCaches {
    layer_backdrop_cache: HashMap<String, CachedBackdropTexture>,
    layer_framebuffer_effect_states: HashMap<String, ShaderEffectElementState>,
    layer_effect_cache: HashMap<String, WindowEffectElementState>,
    popup_effect_cache: HashMap<String, WindowEffectElementState>,
    popup_framebuffer_effect_states: HashMap<String, ShaderEffectElementState>,
}

#[derive(Default)]
struct WindowCaches {
    shader_cache: HashMap<String, ShaderEffectElementState>,
    backdrop_cache: HashMap<String, CachedBackdropTexture>,
    window_effect_cache: HashMap<String, WindowEffectElementState>,
}

/// Caches of the targets not being built right now, by scope.
#[derive(Default)]
pub struct ScopeCaches {
    /// Scope → window id → caches.
    windows: HashMap<String, HashMap<String, WindowCaches>>,
    layers: HashMap<String, StoredLayerCaches>,
    /// Scopes being built, innermost last; the output's own scope is implied below.
    stack: Vec<String>,
}

type Decorations = HashMap<smithay::desktop::Window, crate::ssd::WindowDecorationState>;

impl ScopeCaches {
    /// Start building `scope`: put the current target's caches away and
    /// install `scope`'s.
    pub fn enter(
        &mut self,
        scope: &str,
        output_name: &str,
        decorations: &mut Decorations,
        layers: &mut LayerCaches<'_>,
    ) {
        let current = self
            .stack
            .last()
            .cloned()
            .unwrap_or_else(|| output_name.to_owned());
        self.stash(&current, decorations, layers);
        self.install(scope, decorations, layers);
        self.stack.push(scope.to_owned());
    }

    /// Done building `scope`: put its caches away and reinstall the caches of
    /// the target that was being built before.
    pub fn leave(
        &mut self,
        scope: &str,
        output_name: &str,
        decorations: &mut Decorations,
        layers: &mut LayerCaches<'_>,
    ) {
        self.stash(scope, decorations, layers);
        self.stack.pop();
        let previous = self
            .stack
            .last()
            .cloned()
            .unwrap_or_else(|| output_name.to_owned());
        self.install(&previous, decorations, layers);
    }

    fn stash(&mut self, scope: &str, decorations: &mut Decorations, layers: &mut LayerCaches<'_>) {
        let windows = self.windows.entry(scope.to_owned()).or_default();
        for decoration in decorations.values_mut() {
            let caches = WindowCaches {
                shader_cache: std::mem::take(&mut decoration.shader_cache),
                backdrop_cache: std::mem::take(&mut decoration.backdrop_cache),
                window_effect_cache: std::mem::take(&mut decoration.window_effect_cache),
            };
            windows.insert(decoration.snapshot.id.clone(), caches);
        }
        self.layers.insert(
            scope.to_owned(),
            StoredLayerCaches {
                layer_backdrop_cache: std::mem::take(layers.layer_backdrop_cache),
                layer_framebuffer_effect_states: std::mem::take(
                    layers.layer_framebuffer_effect_states,
                ),
                layer_effect_cache: std::mem::take(layers.layer_effect_cache),
                popup_effect_cache: std::mem::take(layers.popup_effect_cache),
                popup_framebuffer_effect_states: std::mem::take(
                    layers.popup_framebuffer_effect_states,
                ),
            },
        );
    }

    fn install(
        &mut self,
        scope: &str,
        decorations: &mut Decorations,
        layers: &mut LayerCaches<'_>,
    ) {
        let mut windows = self.windows.remove(scope).unwrap_or_default();
        for decoration in decorations.values_mut() {
            if let Some(caches) = windows.remove(&decoration.snapshot.id) {
                decoration.shader_cache = caches.shader_cache;
                decoration.backdrop_cache = caches.backdrop_cache;
                decoration.window_effect_cache = caches.window_effect_cache;
            }
        }
        let stored = self.layers.remove(scope).unwrap_or_default();
        *layers.layer_backdrop_cache = stored.layer_backdrop_cache;
        *layers.layer_framebuffer_effect_states = stored.layer_framebuffer_effect_states;
        *layers.layer_effect_cache = stored.layer_effect_cache;
        *layers.popup_effect_cache = stored.popup_effect_cache;
        *layers.popup_framebuffer_effect_states = stored.popup_framebuffer_effect_states;
    }

    /// Release the caches of render textures `output_name`'s plan no longer
    /// has, and of windows that are gone.
    pub fn retain_scopes(
        &mut self,
        output_name: &str,
        plan: &super::composition::OutputComposition,
        decorations: &Decorations,
    ) {
        if self.windows.is_empty() && self.layers.is_empty() {
            return;
        }
        let prefix = format!("{output_name}#");
        let keep: HashSet<String> = plan
            .textures
            .iter()
            .map(|texture| format!("{prefix}{}", texture.key))
            .collect();
        let live = |scope: &String| !scope.starts_with(&prefix) || keep.contains(scope);
        self.windows.retain(|scope, _| live(scope));
        self.layers.retain(|scope, _| live(scope));
        let window_ids: HashSet<&str> = decorations
            .values()
            .map(|decoration| decoration.snapshot.id.as_str())
            .collect();
        for windows in self.windows.values_mut() {
            windows.retain(|id, _| window_ids.contains(id.as_str()));
        }
    }
}
