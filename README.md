<div align="center">
<h2>ShojiWM</h2>
<p>A highly customizable Wayland compositor configured with TypeScript/TSX.</p>

<a href="https://discord.gg/NheBbu3FX6" data-size="large">
  <img alt="Discord" src="https://img.shields.io/discord/1516819976496091318.svg?label=Discord&logo=Discord&colorB=7289da&style=for-the-badge">
</a>



<video src="https://github.com/user-attachments/assets/b93d7afa-87cd-4399-8603-d5c6a3d63ebf" width="320" height="240" controls></video>
</div>

## Documents

- [English](https://bea4dev.github.io/ShojiWM/)
- [日本語](https://bea4dev.github.io/ShojiWM/ja/)

## Quick Start

Source installs are documented in the getting started guide. Experimental NixOS
support is also available:

```bash
nix develop
cargo run --release -p shoji_wm -- --dev
```

For NixOS module installation, see the
[installation docs](https://bea4dev.github.io/ShojiWM/docs/getting-started/installation).

## Features

- [x] Window management
- [x] Animations
- [x] Screenshots and screen sharing via xdg-desktop-portal-shojiwm
- [x] XWayland support via [xwayland-satellite](https://github.com/Supreeeme/xwayland-satellite)
- [x] Custom shaders
- [x] Layer shell support
- [x] Multi-monitor support
- [x] Intel, AMD, and NVIDIA GPU support

## Why not Niri or Hyprland?

Niri and Hyprland are, at their core, software that bundles a window manager and a compositor together.

ShojiWM is different. It provides only the compositor, plus window-manager functionality as a default config.

In other words, the window-manager part is something you can program entirely and freely yourself. That is exactly why it bills itself as "The most customizable Wayland compositor with TypeScript (tsx)."

Here is an example. The code below implements a window's close button. When you run it, the button's composition changes reactively on hover, so its appearance updates.

<img width="720" height="406" alt="Image" src="https://github.com/user-attachments/assets/0a2a95ef-50b3-40bb-83eb-28be6d078a79" />

```tsx
const CloseButton = ({ window }: { window: WaylandWindow }) => {
  const [hover, setHover] = useState(false);

  const borderColor = hover((hover) => (hover ? "#00000000" : "#F0808030"));

  var icon: CompositionRenderable | null = null;
  if (hover()) {
    icon = (
      <Image
        src="./assets/x.svg"
        style={{
          width: 16,
          height: 16,
          position: "absolute",
          zIndex: 1,
          pointerEvents: "none",
        }}
      />
    );
  }

  return (
    <Box style={{ position: "relative", flexShrink: 0 }}>
      <Button
        onHoverChange={setHover}
        style={{
          width: 16,
          height: 16,
          borderRadius: 8,
          background: "#FFFFFF20",
          border: { px: 1, color: borderColor },
        }}
        onClick={window.close}
      />
      {icon}
    </Box>
  );
};
```

## How ShojiWM compares

How ShojiWM differs from two popular Wayland compositors, [Niri](https://github.com/YaLTeR/niri)
and [Hyprland](https://github.com/hyprwm/Hyprland).

**Legend:** ✅ Yes / built-in &nbsp;·&nbsp; 🟡 Partial / limited &nbsp;·&nbsp; ❌ No

| Capability                                                    |      Niri       |    Hyprland     | ShojiWM |
| ------------------------------------------------------------- | :-------------: | :-------------: | :-----: |
| Server-side decoration (SSD) customization via a standard API |       ❌        |       ❌        |   ✅    |
| Build your own window-management strategy in TypeScript       |       ❌        | 🟡 <sup>1</sup> |   ✅    |
| Powerful custom shader pipeline API                           | 🟡 <sup>2</sup> | 🟡 <sup>3</sup> |   ✅    |
| Linux gaming support, including tearing                       | 🟡 <sup>4</sup> |       ✅        |   ✅    |
| First-class xwayland-satellite support                        |       ✅        |       ❌        |   ✅    |

<sup>1</sup> Hyprland 0.55+ adds custom layouts and event scripting via Lua (not TypeScript); core WM behavior remains built-in.

<sup>2</sup> Custom GLSL is limited to window open/close/resize animations.

<sup>3</sup> A single full-screen screen shader, not a per-element pipeline.

<sup>4</sup> Niri supports VRR (adaptive sync), but not a tearing / immediate-flip mode.

> Comparison reflects each project at the time of writing; corrections are welcome.
