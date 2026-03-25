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

WINDOW_MANAGER.effect.background_effect = {
    effect: compileEffect({
        input: xrayBackdropSource(),
        invalidate: {
            kind: "on-source-damage-box",
            antiArtifactMargin: 12,
        },
        pipeline: [
            dualKawaseBlur({ radius: 4, passes: 3 }),
        ],
    }),
};

WINDOW_MANAGER.event.onOpen((window) => {
    window.setCloseAnimationDuration(seconds(0.5));
    window.animation.start(openAnimation, {
        duration: seconds(0.5),
        to: 1,
        easing: cubicBezier(0.1, 0.93, 0.1, 0.93)
    });
    window.animation.set(focusAnimation, window.isFocused() ? 1 : 0.8);
    window.animation.start(testAnimation, { duration: seconds(100) })
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

const testAnimation = animationVariable("test")

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
    const titlebarBackground = isFocused ? "#1f2430" : "#2a2f3a";
    const titleColor = isFocused ? "#f5f7fa" : "#c9d1d9";

    const titlebarStyle: SSDStyle = {
        height: 30,
        paddingX: 20,
        gap: 8,
        alignItems: "center",
        background: titlebarBackground,
    };

    const test = window.animation.signal(testAnimation)

    const backgroundShader = compileEffect({
        input: shaderInput(loadShader("./rainbow-test.frag"), { uniforms: { phase_01: test(t => t > 0.1 ? 1 : t), speed: 100 } }),
        invalidate: {
            kind: "on-source-damage-box",
            antiArtifactMargin: 0,
        },
        pipeline: [
            /*
            noise({ kind: "salt", amount: 0.01 }),
            dualKawaseBlur({ radius: 4, passes: 3 }),
            shaderStage(loadShader("./liquid-glass.frag"), {
                uniforms: {
                    inset_px: 0.0,
                    border_radius_px: 20.0,
                    edge_width_px: 15.0,
                    edge_softness_px: 0.0,
                    max_warp_px: 40.0,
                    interior_warp_px: 0.0,
                    white_tint: 0.0,
                    edge_highlight: 0.0,
                },
            }),*/
            //shaderStage(loadShader("./blur.frag"))
            //shaderStage(loadShader("./rainbow-test.frag"), { uniforms: { phase_01: test(t => t > 0.1 ? 1 : t), speed: 100 } }),
        ],
    });

    return (
        <WindowBorder
            style={{
                border: { px: 2, color: borderColor },
                borderRadius: 20,
                //background: "#101319",
            }}
        >
            <Box direction="column">
                <ShaderEffect shader={backgroundShader} direction="row" style={titlebarStyle}>
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
                </ShaderEffect>
                <ClientWindow />
            </Box>
        </WindowBorder>
    );
};

export { WINDOW_MANAGER };
