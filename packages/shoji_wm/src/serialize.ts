import type {
  DecorationChild,
  DecorationElementNode,
  SerializableDecorationChild,
  SerializedDecorationNode,
  WindowActionDescriptor,
} from "./types";
import { isSignal } from "./signals";
import {
  enterWindowNodeDependencyScope,
  leaveWindowNodeDependencyScope,
} from "./runtime-hooks";

export class DecorationSerializationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DecorationSerializationError";
  }
}

export interface DecorationSerializationContext {
  registerClickHandler(key: string, handler: () => void): string;
}

export function serializeDecorationTree(
  node: DecorationChild,
  context?: DecorationSerializationContext,
  path = "root",
): SerializableDecorationChild {
  if (typeof node === "string" || typeof node === "number") {
    return node;
  }

  return serializeElementNode(node, context, path);
}

export function patchSerializedDecorationTree(
  node: DecorationChild,
  previous: SerializableDecorationChild,
  dirtyNodeIds: ReadonlySet<string>,
  context?: DecorationSerializationContext,
  path = "root",
): SerializableDecorationChild {
  if (typeof node === "string" || typeof node === "number") {
    return previous;
  }
  if (typeof previous === "string" || typeof previous === "number") {
    return serializeDecorationTree(node, context, path);
  }

  const shouldReplaceSelf = dirtyNodeIds.has(path);
  if (shouldReplaceSelf) {
    return serializeElementNode(node, context, path);
  }

  return {
    kind: previous.kind,
    nodeId: previous.nodeId,
    props: previous.props,
    children: node.children.map((child, index) => {
      const childPath = childNodePath(path, child, index);
      const previousChild = previous.children[index];
      if (previousChild === undefined) {
        return serializeDecorationTree(child, context, childPath);
      }
      return patchSerializedDecorationTree(child, previousChild, dirtyNodeIds, context, childPath);
    }),
  };
}

function serializeElementNode(
  node: DecorationElementNode,
  context?: DecorationSerializationContext,
  path = "root",
): SerializedDecorationNode {
  enterWindowNodeDependencyScope(path);
  try {
    return {
      kind: node.type,
      nodeId: path,
      props: serializeProps(node.props, context, path),
      children: node.children.map((child, index) =>
        serializeDecorationTree(child, context, childNodePath(path, child, index))
      ),
    };
  } finally {
    leaveWindowNodeDependencyScope();
  }
}

function serializeProps(
  props: Record<string, unknown>,
  context?: DecorationSerializationContext,
  path = "root",
): Record<string, unknown> {
  const serialized: Record<string, unknown> = {};

  for (const [key, value] of Object.entries(props)) {
    if (value === undefined) {
      continue;
    }

    if (key === "onClick") {
      serialized.onClick = serializeOnClick(
        value,
        context,
        typeof props.id === "string" ? `${path}#${props.id}` : `${path}.onClick`,
      );
      continue;
    }

    if (isSignal(value)) {
      serialized[key] = serializeValue(value);
      continue;
    }

    if (typeof value === "function") {
      throw new DecorationSerializationError(
        `function prop "${key}" is not serializable`,
      );
    }

    serialized[key] = serializeValue(value);
  }

  return serialized;
}

function serializeOnClick(
  value: unknown,
  context?: DecorationSerializationContext,
  handlerKey?: string,
): unknown {
  if (isWindowActionDescriptor(value)) {
    return value.action;
  }

  if (typeof value === "function") {
    if (!context) {
      throw new DecorationSerializationError(
        "onClick function handlers require a serialization context",
      );
    }
    if (!handlerKey) {
      throw new DecorationSerializationError(
        "onClick function handlers require a stable handler key",
      );
    }

    return {
      kind: "runtime-handler",
      id: context.registerClickHandler(handlerKey, value as () => void),
    };
  }

  if (value == null) {
    return undefined;
  }

  throw new DecorationSerializationError(
    "onClick must be a serializable window action descriptor or runtime handler",
  );
}

function serializeValue(value: unknown): unknown {
  if (isSignal(value)) {
    return serializeValue(value());
  }

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
      if (isSignal(nested)) {
        serialized[key] = serializeValue(nested());
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

function childNodePath(
  parentPath: string,
  child: DecorationChild,
  index: number,
): string {
  if (typeof child === "string" || typeof child === "number") {
    return `${parentPath}.primitive[${index}]`;
  }

  if (child.key != null) {
    return `${parentPath}.${child.type}#${String(child.key)}`;
  }

  return `${parentPath}.${child.type}[${index}]`;
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
