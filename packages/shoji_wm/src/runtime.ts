import type {
  ComponentProps,
  DecorationChild,
  DecorationElementNode,
  DecorationNodeType,
} from "./types";

export function createElementNode(
  type: DecorationNodeType,
  props: ComponentProps = {},
  key?: string | number | null,
): DecorationElementNode {
  const { children, ...rest } = props;

  return {
    kind: "element",
    type,
    key: key ?? null,
    props: rest,
    children: normalizeChildren(children),
  };
}

export function normalizeChildren(children: unknown): DecorationChild[] {
  if (children == null || children === false || children === true) {
    return [];
  }

  if (Array.isArray(children)) {
    return children.flatMap(normalizeChildren);
  }

  return [children as DecorationChild];
}
