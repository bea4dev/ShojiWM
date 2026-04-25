import {
    AppIcon,
    Box,
    Button,
    ClientWindow,
    Image,
    ShaderEffect,
    Label,
    WINDOW_MANAGER,
    WindowBorder,
    backdropSource,
    compileEffect,
    dualKawaseBlur,
    type SSDStyle,
    type WaylandWindow,
    animationVariable,
    seconds,
    cubicBezier,
    useState,
    computed,
    shaderInput,
    shaderStage,
    loadShader,
} from "shoji_wm";
import type { DecorationRenderable, Direction, MaybeSignal } from "shoji_wm/types";

const NOCTALIA_SHELL_PATH = "/home/bea4dev/Documents/development/noctalia-shell-shojiwm";

/*
WINDOW_MANAGER.output.applyDisplayConfig((display) => {
    for (let displayName of WINDOW_MANAGER.output.list) {
        display[displayName] = {
            resolution: "best",
            position: "auto",
            scale: 2,
        };
    }
});*/

WINDOW_MANAGER.process.once("fcitx5", {
    command: ["fcitx5", "-d"],
    runPolicy: "once-per-session",
});
WINDOW_MANAGER.process.once("shell", {
    command: ["qs", "--path", NOCTALIA_SHELL_PATH],
    runPolicy: "once-per-session",
});


WINDOW_MANAGER.key.bind("terminal", "Super+T", () => {
    WINDOW_MANAGER.process.spawn({ command: ["kitty"] });
});
WINDOW_MANAGER.key.bind("launcher", "Super+A", () => {
    WINDOW_MANAGER.process.spawn({ command: ["qs", "--path", NOCTALIA_SHELL_PATH, "ipc", "call", "launcher", "toggle"] });
});
WINDOW_MANAGER.key.bind("clipboard", "Super+V", () => {
    WINDOW_MANAGER.process.spawn({ command: ["qs", "--path", NOCTALIA_SHELL_PATH, "ipc", "call", "launcher", "clipboard"] });
});


WINDOW_MANAGER.output.applyDisplayConfig((display) => {
    display["eDP-1"] = {
        resolution: "best",
        position: "auto",
        scale: 1.25,
    };
    display["DP-4"] = {
        resolution: "best",
        position: "auto",
        scale: 1.5,
    };
    display["DP-2"] = {
        resolution: "best",
        position: "auto",
        scale: 1.6,
    };
});

const openAnimation = animationVariable("window.open")
const focusAnimation = animationVariable("window.focus");

WINDOW_MANAGER.effect.background_effect = compileEffect({
    input: backdropSource(),
    invalidate: { kind: "on-source-damage-box", antiArtifactMargin: 8 },
    pipeline: [
        dualKawaseBlur({ radius: 4, passes: 2 }),
    ]
});

WINDOW_MANAGER.event.onOpen((window) => {
    window.setCloseAnimationDuration(seconds(2.0));
    window.animation.start(openAnimation, {
        duration: seconds(2.0),
        to: 1,
        easing: cubicBezier(0.1, 0.93, 0.1, 0.93)
    });
    window.animation.set(focusAnimation, window.isFocused() ? 1 : 1);
});

WINDOW_MANAGER.event.onStartClose((window) => {
    window.animation.start(openAnimation, {
        duration: seconds(2.0),
        to: 0,
        easing: cubicBezier(0.1, 0.93, 0.1, 0.93)
    });
});

WINDOW_MANAGER.event.onFocus((window, focused) => {
    /*
    window.animation.start(focusAnimation, {
        duration: seconds(0.5),
        to: focused ? 1 : 0.9,
        easing: cubicBezier(0.1, 0.93, 0.1, 0.93)
    });*/
});

WINDOW_MANAGER.decoration = (window: WaylandWindow) => {
    const scale = window.animation.signal(focusAnimation);
    const openVariable = window.animation.signal(openAnimation);
    const opacity = openVariable;
    const translateY = openVariable(variable => (1 - variable) * 200);

    window.transform.origin = { x: 0.5, y: 0.5 };
    window.transform.translateX = 0;
    window.transform.translateY = translateY;
    window.transform.scaleX = scale;
    window.transform.scaleY = scale;
    window.transform.opacity = opacity;

    const borderColor = window.isFocused(focused => focused ? "#d7ba7d" : "#4f5666");
    const titlebarBackground = window.isFocused(focused => focused ? "#1f243080" : "#2a2f3a80");
    const titleColor = window.isFocused(focused => focused ? "#f5f7fa" : "#c9d1d9");

    const titlebarStyle: SSDStyle = {
        height: 30,
        paddingX: 8,
        gap: 8,
        alignItems: "center",
        background: titlebarBackground,
    };

    const backgroundShader = compileEffect({
        input: backdropSource(),
        invalidate: { kind: "on-source-damage-box", antiArtifactMargin: 8 },
        pipeline: [
            dualKawaseBlur({ radius: 4, passes: 2 }),
            shaderStage(loadShader("./src/liquid-glass.frag"), {
                uniforms: {
                    inset_px: 0.0,
                    border_radius_px: 10.0,
                    edge_width_px: 10.0,
                    edge_softness_px: 0.0,
                    max_warp_px: 20.0,
                    interior_warp_px: 0.0,
                    white_tint: 0.0,
                    edge_highlight: 0.0,
                },
            }),
        ],
    });

    return (
        <WindowBorder
            style={{
                border: { px: 2, color: borderColor },
                borderRadius: 10,
                background: "#10131900",
                padding: 0,
                paddingX: 0,
                paddingRight: 0,
            }}
        >
            <Box direction="row">
                <Box direction="column">
                    <ShaderEffect shader={backgroundShader} direction="row" style={titlebarStyle}>
                        <AppIcon icon={window.icon} style={{ width: 16, height: 16 }} />
                        <Label
                            text={window.title}
                            style={{
                                color: titleColor,
                                fontFamily: ["Noto Sans CJK JP", "Noto Color Emoji"],
                                fontSize: 13,
                                fontWeight: 600,
                                flexGrow: 1,
                                flexShrink: 1,
                                minWidth: 0,
                            }}
                        />
                        <CloseButton window={window} />
                    </ShaderEffect>
                    <ClientWindow />
                </Box>
            </Box>
        </WindowBorder>
    );
};

const CloseButton = ({ window }: { window: WaylandWindow }) => {
    const [hover, setHover] = useState(false);

    const background = hover(hover => hover ? "#F08080" : "#F0808080");

    var icon: DecorationRenderable | null = null;
    if (hover()) {
        icon = (
            <Image
                src="./assets/x.svg"
                style={{
                    width: 16,
                    height: 16,
                    position: "absolute",
                    zIndex: 1,
                    pointerEvents: "none"
                }}
            />
        );
    }

    return (
        <Box style={{ position: "relative" }}>
            <Button
                onHoverChange={setHover}
                style={{
                    width: 16,
                    height: 16,
                    borderRadius: 8,
                    background: background,
                    border: { px: 1, color: "#f5f7fa" },
                }}
                onClick={window.close}
            />
            {icon}
        </Box>
    )
};

export { WINDOW_MANAGER };
