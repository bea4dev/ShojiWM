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
    backdropSource,
    compileEffect,
    dualKawaseBlur,
    type SSDStyle,
    type WaylandWindow,
    animationVariable,
    seconds,
    cubicBezier,
    useState,
} from "shoji_wm";

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

WINDOW_MANAGER.output.applyDisplayConfig((display) => {
    display["eDP-1"] = {
        resolution: "best",
        position: "auto",
        scale: 1,
    };
    display["DP-4"] = {
        resolution: "best",
        position: "auto",
        scale: 1.5,
    };
});

const openAnimation = animationVariable("window.open")
const focusAnimation = animationVariable("window.focus");

WINDOW_MANAGER.effect.background_effect = compileEffect({
    input: backdropSource(),
    invalidate: { kind: "on-source-damage-box", antiArtifactMargin: 12 },
    pipeline: [
        dualKawaseBlur({ radius: 0, passes: 0 }),
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

    const borderColor = window.isFocused(focused => focused ? "#d7ba7d" : "#4f5666");
    const titlebarBackground = window.isFocused(focused => focused ? "#1f243080" : "#2a2f3a80");
    const titleColor = window.isFocused(focused => focused ? "#f5f7fa" : "#c9d1d9");

    const titlebarStyle: SSDStyle = {
        height: 30,
        paddingX: 20,
        gap: 8,
        alignItems: "center",
        background: titlebarBackground,
    };

    const backgroundShader = compileEffect({
        input: backdropSource(),
        invalidate: { kind: "on-source-damage-box", antiArtifactMargin: 8 },
        pipeline: [
            dualKawaseBlur({ radius: 0, passes: 0 })
        ],
    });

    return (
        <WindowBorder
            style={{
                border: { px: 2, color: borderColor },
                borderRadius: 20,
                background: "#10131900",
            }}
        >
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
                    <TestComponent />
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
                        onClick={window.close}
                    />
                </ShaderEffect>
                <ClientWindow />
            </Box>
        </WindowBorder>
    );
};

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
