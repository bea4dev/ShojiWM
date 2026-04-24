import {
  AppIcon,
  Box,
  Button,
  ClientWindow,
  Label,
  WindowBorder,
  computed,
  type SSDStyle,
  type WaylandWindow,
  useState,
  windowAction,
} from "./index";

const TITLEBAR_HEIGHT = 30;

export const defaultWindowDecoration = (window: WaylandWindow) => {
  const isFocused = window.isFocused();
  const [closeHovered, setCloseHovered] = useState(false);
  const [closeActive, setCloseActive] = useState(false);

  const borderColor = isFocused ? "#d7ba7d" : "#4f5666";
  const titlebarBackground = isFocused ? "#1f2430" : "#2a2f3a";
  const titleColor = isFocused ? "#f5f7fa" : "#c9d1d9";
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
    height: TITLEBAR_HEIGHT,
    paddingX: 20,
    gap: 8,
    alignItems: "center",
    background: titlebarBackground,
  };

  return (
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
              fontSize: 13,
              fontWeight: 600,
            }}
          />
          <Box style={{ flexGrow: 1 }} />
          <Button
            onHoverChange={setCloseHovered}
            onActiveChange={setCloseActive}
            style={{
              width: 18,
              height: 18,
              borderRadius: 9,
              background: closeBackground,
              border: window.isFocused((focused) => ({
                px: focused ? 1 : 0,
                color: "#f5f7fa",
              })),
            }}
            onClick={windowAction("close")}
          />
        </Box>
        <ClientWindow />
      </Box>
    </WindowBorder>
  );
};
