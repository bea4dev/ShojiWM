import {
    AppIcon,
    Box,
    Button,
    ClientWindow,
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
    const [closeHovered, setCloseHovered] = useState(false);
    const [closeActive, setCloseActive] = useState(false);

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
    const closeBackground = computed(() => {
        if (closeActive()) {
            return "#d63b3b";
        }
        if (closeHovered()) {
            return "#b32626";
        }
        return "#8a1c1c";
    });

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
            shaderStage(loadShader("./liquid-glass.frag"), {
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
                borderRadius: 20,
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
                            }}
                        />
                        <Box style={{ flexGrow: 1 }} />
                        <TestComponent2 />
                        <TestComponent />
                        <Button
                            onHoverChange={setCloseHovered}
                            onActiveChange={setCloseActive}
                            style={{
                                width: 18,
                                height: 18,
                                borderRadius: 9,
                                background: closeBackground,
                                border: window.isFocused(focused => ({
                                    px: focused ? 1 : 0,
                                    color: "#f5f7fa",
                                })),
                            }}
                            onClick={window.close}
                        />
                    </ShaderEffect>
                    <ClientWindow />
                </Box>
            </Box>
        </WindowBorder>
    );
};

const TestComponent2 = () => {
    const [state, setState] = useState(0);

    return (
        <Box
            style={{
                position: "relative",
                alignItems: "center",
                paddingLeft: 12,
                paddingRight: 12,
            }}
            direction="row"
        >
            <Label
                text={state(state => state.toString())}
                style={{
                    marginLeft: 32,
                    color: "#ffffff",
                }}
            />
            <Box
                style={{
                    position: "absolute",
                    width: 96,
                    height: 18,
                    borderRadius: 2,
                    background: "#000000",
                    zIndex: 5,
                    pointerEvents: "none",
                    opacity: 0.5,
                }}
            />
            <Button
                onClick={() => { setState(state() + 1) }}
                style={{
                    position: "absolute",
                    width: 18,
                    height: 18,
                    borderRadius: 9,
                    background: "#ff5f57",
                    zIndex: 10,
                    pointerEvents: "auto",
                    opacity: 0.5,
                    transform: { translateY: 2 }
                }}
            />
        </Box>
    );
}

const TestComponent = () => {
    const [state, setState] = useState(0);

    if (state() > 10) {
        return null;
    }

    return (
        <Box direction="horizontal" style={{ gap: 6 }}>
            <Label text={state(state => state.toString())} style={{ fontFamily: "Noto Sans CJK JP" }} />
            <Button
                onClick={() => { setState(state() + 1); }}
                style={{
                    width: 18,
                    height: 18,
                    borderRadius: 9,
                    border: { px: 2, color: "#FFFFFF" },
                    background: "#000000",
                }}
            />
        </Box>
    );
};

export { WINDOW_MANAGER };
