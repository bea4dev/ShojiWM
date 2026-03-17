/** @jsxImportSource shoji_wm */

import {
  AppIcon,
  applyInteractionStyle,
  Box,
  Button,
  getInteractionState,
  Label,
  Window,
  WindowBorder,
  type SSDStyle,
  type WaylandWindow,
  windowAction,
} from "./index";

const TITLEBAR_HEIGHT = 30;

export const defaultWindowDecoration = (window: WaylandWindow) => {
  const isFocused = window.isFocused;
  const closeState = getInteractionState(window, "window.close");

  const borderColor = isFocused ? "#d7ba7d" : "#4f5666";
  const titlebarBackground = isFocused ? "#1f2430" : "#2a2f3a";
  const titleColor = isFocused ? "#f5f7fa" : "#c9d1d9";

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
        border: { px: 1, color: borderColor },
        borderRadius: 20,
        background: "#101319",
      }}
    >
      <Box direction="column">
        <Box direction="row" style={titlebarStyle}>
          <AppIcon icon={window.icon} style={{ width: 16, height: 16 }} />
          <Label
            text={window.title}
            style={{
              color: titleColor,
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
            onClick={windowAction("close")}
          />
        </Box>
        <Window />
      </Box>
    </WindowBorder>
  );
};
