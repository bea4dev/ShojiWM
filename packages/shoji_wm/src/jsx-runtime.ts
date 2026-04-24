import { createElementNode, normalizeChildren, renderComponent } from "./runtime";
import type {
  Component,
  ComponentProps,
  ClientWindowProps,
  ImageProps,
  ShaderEffectProps,
  DecorationChild,
  DecorationRenderable,
  DecorationNodeType,
} from "./types";

export function jsx(
  type: DecorationNodeType | Component<any>,
  props: ComponentProps,
  key?: string,
): DecorationRenderable {
  return createJsxNode(type, props, key);
}

export function jsxs(
  type: DecorationNodeType | Component<any>,
  props: ComponentProps,
  key?: string,
): DecorationRenderable {
  return createJsxNode(type, props, key);
}

export const Fragment = "Fragment" satisfies DecorationNodeType;

function createJsxNode(
  type: DecorationNodeType | Component<any>,
  props: ComponentProps = {},
  key?: string,
): DecorationRenderable {
  const normalizedProps = {
    ...props,
    children: normalizeChildren(props.children),
  };

  if (typeof type === "function") {
    return renderComponent(type, normalizedProps, key ?? null);
  }

  return createElementNode(type, normalizedProps, key);
}

export namespace JSX {
  export type Element = DecorationRenderable;
  export type ElementType = DecorationNodeType | Component<any>;
  export interface ElementChildrenAttribute {
    children: {};
  }
  export interface IntrinsicAttributes {
    key?: string | number;
  }
  export interface IntrinsicElements {
    Box: ComponentProps;
    Label: ComponentProps;
    Button: ComponentProps;
    AppIcon: ComponentProps;
    Image: ImageProps;
    ShaderEffect: ShaderEffectProps;
    ClientWindow: ClientWindowProps;
    Window: ComponentProps;
    WindowBorder: ComponentProps;
    Fragment: ComponentProps;
  }
}

export type { DecorationChild };
