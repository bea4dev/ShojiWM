import {
    AppIcon,
    applyInteractionStyle,
    Box,
    Button,
    ClientWindow,
    ShaderEffect,
    getInteractionState,
    Label,
    WINDOW_MANAGER,
    WindowBorder,
    windowAction,
    backdropSource,
    compileEffect,
    dualKawaseBlur,
    loadShader,
    noise,
    shaderStage,
    type SSDStyle,
    type WaylandWindow,
    signal,
    animationVariable,
    seconds,
    cubicBezier,
    save,
    unit,
    get,
    blend,
    xrayBackdropSource,
    shaderInput
} from "shoji_wm";

const openAnimation = animationVariable("window.open")
const focusAnimation = animationVariable("window.focus");
const electricAnimation = animationVariable("window.electric");


WINDOW_MANAGER.effect.background_effect = compileEffect({
    input: backdropSource(),
    invalidate: { kind: "on-source-damage-box", antiArtifactMargin: 12 },
    pipeline: [
        dualKawaseBlur({ radius: 4, passes: 3 }),
    ]
});

WINDOW_MANAGER.event.onOpen((window) => {
    window.setCloseAnimationDuration(seconds(0.5));
    window.animation.start(openAnimation, {
        duration: seconds(0.5),
        to: 1,
        easing: cubicBezier(0.1, 0.93, 0.1, 0.93)
    });
    window.animation.set(focusAnimation, window.isFocused() ? 1 : 0.8);
    window.animation.start(electricAnimation, { duration: seconds(1.35), repeat: "loop" })
});

WINDOW_MANAGER.event.onStartClose((window) => {
    window.animation.start(openAnimation, {
        duration: seconds(0.5),
        to: 0,
        easing: cubicBezier(0.1, 0.93, 0.1, 0.93)
    });
});

WINDOW_MANAGER.event.onFocus((window, focused) => {
    if (window.animation.running(openAnimation)) {
        return;
    }

    window.animation.start(focusAnimation, {
        duration: seconds(0.5),
        to: focused ? 1 : 0.9,
        easing: cubicBezier(0.1, 0.93, 0.1, 0.93)
    });
});

WINDOW_MANAGER.decoration = (window: WaylandWindow) => {
    const isFocused = window.isFocused();
    const closeState = getInteractionState(window, "window.close");

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

    const borderColor = isFocused ? "#d7ba7d" : "#4f5666";
    const titlebarBackground = isFocused ? "#1f243080" : "#2a2f3a80";
    const titleColor = isFocused ? "#f5f7fa" : "#c9d1d9";

    const titlebarStyle: SSDStyle = {
        height: 30,
        paddingX: 20,
        gap: 8,
        alignItems: "center",
        background: titlebarBackground,
    };

    const electric = window.animation.signal(electricAnimation);
    const electricBorder = compileEffect({
        input: shaderInput(loadShader("./electric-frame.frag"), {
            uniforms: {
                phase_01: electric,
                speed: 1.0,
                radius_px: 24.0,
                frame_width_px: 4.0,
                glow_px: 14.0,
                intensity: scale(value => {
                    const normalized = Math.max(0, Math.min(1, (value - 0.9) / 0.1));
                    return 0.45 + normalized * 0.55;
                }),
            },
        }),
        invalidate: { kind: "always" },
        pipeline: [],
    });

    return (
        <ShaderEffect
            shader={electricBorder}
            direction="column"
            style={{
                padding: 6,
                borderRadius: 24,
                background: "#00000000",
            }}
        >
            <WindowBorder
                style={{
                    border: { px: 2, color: borderColor },
                    borderRadius: 20,
                    background: "#101319",
                }}
            >
                <Box direction="column">
                    <Box direction="row" style={titlebarStyle}>
                        <AppIcon icon={window.icon()} style={{ width: 16, height: 16 }} />
                        <Label
                            text={window.title()}
                            style={{
                                color: titleColor,
                                fontFamily: ["Noto Sans CJK JP", "Noto Color Emoji"],
                                fontSize: 13,
                                fontWeight: 600,
                            }}
                        />
                        <Box style={{ flexGrow: 1 }} />
                        <Button
                            id="window.close"
                            style={applyInteractionStyle(
                                {
                                    width: 18,
                                    height: 18,
                                    borderRadius: 9,
                                    background: "#8a1c1c",
                                },
                                {
                                    hovered: { background: "#b32626" },
                                    active: { background: "#d63b3b" },
                                    focused: { border: { px: 1, color: "#f5f7fa" } },
                                },
                                closeState,
                            )}
                            onClick={() => window.close()}
                        />
                    </Box>
                    <ClientWindow />
                </Box>
            </WindowBorder>
        </ShaderEffect>
    );
};

export { WINDOW_MANAGER };
