import type {
  DecorationChild,
  DecorationElementNode,
  SerializableDecorationChild,
  SerializedDecorationNode,
  WindowActionDescriptor,
} from "./types";

export class DecorationSerializationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DecorationSerializationError";
  }
}

export function serializeDecorationTree(
  node: DecorationChild,
): SerializableDecorationChild {
  if (typeof node === "string" || typeof node === "number") {
    return node;
  }

  return serializeElementNode(node);
}

function serializeElementNode(
  node: DecorationElementNode,
): SerializedDecorationNode {
  return {
    kind: node.type,
    props: serializeProps(node.props),
    children: node.children.map(serializeDecorationTree),
  };
}

function serializeProps(
  props: Record<string, unknown>,
): Record<string, unknown> {
  const serialized: Record<string, unknown> = {};

  for (const [key, value] of Object.entries(props)) {
    if (value === undefined) {
      continue;
    }

    if (typeof value === "function") {
      throw new DecorationSerializationError(
        `function prop "${key}" is not serializable; use a window action descriptor instead`,
      );
    }

    if (key === "onClick") {
      serialized.onClick = serializeOnClick(value);
      continue;
    }

    serialized[key] = serializeValue(value);
  }

  return serialized;
}

function serializeOnClick(value: unknown): unknown {
  if (isWindowActionDescriptor(value)) {
    return value.action;
  }

  if (value == null) {
    return undefined;
  }

  throw new DecorationSerializationError(
    "onClick must be a serializable window action descriptor at this stage",
  );
}

function serializeValue(value: unknown): unknown {
  if (
    value == null ||
    typeof value === "string" ||
    typeof value === "number" ||
    typeof value === "boolean"
  ) {
    return value;
  }

  if (Array.isArray(value)) {
    return value.map(serializeValue);
  }

  if (typeof value === "object") {
    const objectValue = value as Record<string, unknown>;
    const serialized: Record<string, unknown> = {};
    for (const [key, nested] of Object.entries(objectValue)) {
      if (nested === undefined) {
        continue;
      }
      if (typeof nested === "function") {
        throw new DecorationSerializationError(
          `function value at "${key}" is not serializable`,
        );
      }
      serialized[key] = serializeValue(nested);
    }
    return serialized;
  }

  throw new DecorationSerializationError(
    `unsupported prop value type: ${typeof value}`,
  );
}

function isWindowActionDescriptor(
  value: unknown,
): value is WindowActionDescriptor {
  return (
    typeof value === "object" &&
    value !== null &&
    (value as WindowActionDescriptor).kind === "window-action" &&
    typeof (value as WindowActionDescriptor).action === "string"
  );
}
