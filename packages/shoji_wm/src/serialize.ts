import type {
  CompositionChild,
  CompositionElementNode,
  SerializableCompositionChild,
  SerializedCompositionNode,
  WindowActionDescriptor,
} from "./types";
import { isSignal } from "./signals";
import {
  enterWindowNodeDependencyScope,
  enterWindowShaderUniformDependencyScope,
  leaveWindowNodeDependencyScope,
  leaveWindowShaderUniformDependencyScope,
} from "./runtime-hooks";

function labelDebugEnabled(): boolean {
  const env = (globalThis as { process?: { env?: Record<string, string | undefined> } })
    .process?.env;
  const value = env?.SHOJI_LABEL_DEBUG;
  return value !== undefined && value !== "" && value !== "0";
}

function debugSerializedLabel(
  path: string,
  props: Record<string, unknown>,
  serialized: Record<string, unknown>,
): void {
  if (!labelDebugEnabled()) {
    return;
  }
  console.info(
    "label-debug serialize-label",
    JSON.stringify({
      path,
      textType: typeof props.text,
      serializedText: serialized.text,
      style: serialized.style,
    }),
  );
}

export class CompositionSerializationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "CompositionSerializationError";
  }
}

export interface CompositionSerializationContext {
  registerClickHandler(key: string, handler: () => void): string;
  registerInteractionHandler(key: string, handler: () => void): string;
  registerShaderUniformBinding(binding: ShaderUniformBinding): void;
}

export interface ShaderUniformBinding {
  key: string;
  nodeId: string;
  stageIndex: number;
  name: string;
  read(): number[] | null;
}

const SHADER_INPUT_STAGE_INDEX = 0xffff_ffff;

export function serializeCompositionTree(
  node: CompositionChild,
  context?: CompositionSerializationContext,
  path = "root",
): SerializableCompositionChild {
  if (typeof node === "string" || typeof node === "number") {
    return node;
  }

  return serializeElementNode(node, context, path);
}

export function patchSerializedCompositionTree(
  node: CompositionChild,
  previous: SerializableCompositionChild,
  dirtyNodeIds: ReadonlySet<string>,
  context?: CompositionSerializationContext,
  path = "root",
): SerializableCompositionChild {
  if (typeof node === "string" || typeof node === "number") {
    return previous;
  }
  if (typeof previous === "string" || typeof previous === "number") {
    return serializeCompositionTree(node, context, path);
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
        return serializeCompositionTree(child, context, childPath);
      }
      return patchSerializedCompositionTree(child, previousChild, dirtyNodeIds, context, childPath);
    }),
  };
}

function serializeElementNode(
  node: CompositionElementNode,
  context?: CompositionSerializationContext,
  path = "root",
): SerializedCompositionNode {
  enterWindowNodeDependencyScope(path);
  try {
    return {
      kind: node.type,
      nodeId: path,
      props: serializeProps(node.props, context, path, node.type),
      children: node.children.map((child, index) =>
        serializeCompositionTree(child, context, childNodePath(path, child, index))
      ),
    };
  } finally {
    leaveWindowNodeDependencyScope();
  }
}

function serializeProps(
  props: Record<string, unknown>,
  context?: CompositionSerializationContext,
  path = "root",
  kind?: string,
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

    if (key === "onHoverChange" || key === "onActiveChange") {
      serialized[key] = serializeInteractionChangeHandler(
        value,
        context,
        typeof props.id === "string" ? `${path}#${props.id}.${key}` : `${path}.${key}`,
        key,
      );
      continue;
    }

    if (kind === "ShaderEffect" && key === "shader") {
      serialized[key] = serializeShaderEffect(value, context, path);
      continue;
    }

    if (isSignal(value)) {
      serialized[key] = serializeValue(value);
      continue;
    }

    if (typeof value === "function") {
      throw new CompositionSerializationError(
        `function prop "${key}" is not serializable`,
      );
    }

    serialized[key] = serializeValue(value);
  }

  if (kind === "Label") {
    debugSerializedLabel(path, props, serialized);
  }

  return serialized;
}

function serializeShaderEffect(
  value: unknown,
  context: CompositionSerializationContext | undefined,
  nodeId: string,
): unknown {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return serializeValue(value);
  }

  const effect = value as Record<string, unknown>;
  const serialized: Record<string, unknown> = {};
  for (const [key, nested] of Object.entries(effect)) {
    if (nested === undefined) {
      continue;
    }
    if (key === "input") {
      serialized.input = serializeShaderInput(nested, context, nodeId);
      continue;
    }
    if (key !== "pipeline" || !Array.isArray(nested)) {
      serialized[key] = serializeValue(nested);
      continue;
    }
    serialized.pipeline = nested.map((stage, stageIndex) =>
      serializeEffectStage(stage, context, nodeId, stageIndex)
    );
  }
  return serialized;
}

function serializeShaderInput(
  value: unknown,
  context: CompositionSerializationContext | undefined,
  nodeId: string,
): unknown {
  return serializeShaderUniformContainer(
    value,
    context,
    nodeId,
    SHADER_INPUT_STAGE_INDEX,
    "shader-input",
  );
}

function serializeEffectStage(
  value: unknown,
  context: CompositionSerializationContext | undefined,
  nodeId: string,
  stageIndex: number,
): unknown {
  return serializeShaderUniformContainer(
    value,
    context,
    nodeId,
    stageIndex,
    "shader-stage",
  );
}

function serializeShaderUniformContainer(
  value: unknown,
  context: CompositionSerializationContext | undefined,
  nodeId: string,
  stageIndex: number,
  expectedKind: "shader-input" | "shader-stage",
): unknown {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return serializeValue(value);
  }

  const stage = value as Record<string, unknown>;
  if (stage.kind !== expectedKind) {
    return serializeValue(value);
  }

  const serialized: Record<string, unknown> = {};
  for (const [key, nested] of Object.entries(stage)) {
    if (nested === undefined) {
      continue;
    }
    if (
      key !== "uniforms" ||
      typeof nested !== "object" ||
      nested === null ||
      Array.isArray(nested)
    ) {
      serialized[key] = serializeValue(nested);
      continue;
    }

    const uniforms: Record<string, unknown> = {};
    for (const [name, uniform] of Object.entries(
      nested as Record<string, unknown>,
    )) {
      const bindingKey = shaderUniformBindingKey(nodeId, stageIndex, name);
      context?.registerShaderUniformBinding({
        key: bindingKey,
        nodeId,
        stageIndex,
        name,
        read: () => readNumericShaderUniform(uniform),
      });
      enterWindowShaderUniformDependencyScope(bindingKey, nodeId);
      try {
        uniforms[name] = serializeValue(uniform);
      } finally {
        leaveWindowShaderUniformDependencyScope();
      }
    }
    serialized.uniforms = uniforms;
  }
  return serialized;
}

function shaderUniformBindingKey(
  nodeId: string,
  stageIndex: number,
  name: string,
): string {
  return `${nodeId}\u0000${stageIndex}\u0000${name}`;
}

function readNumericShaderUniform(value: unknown): number[] | null {
  const entries = Array.isArray(value) ? value : [value];
  if (entries.length < 1 || entries.length > 4) {
    return null;
  }
  const values: number[] = [];
  for (const entry of entries) {
    const resolved = isSignal(entry) ? entry.peek() : entry;
    if (typeof resolved !== "number" || !Number.isFinite(resolved)) {
      return null;
    }
    values.push(resolved);
  }
  return values;
}

function serializeInteractionChangeHandler(
  value: unknown,
  context: CompositionSerializationContext | undefined,
  handlerKey: string,
  propName: string,
): unknown {
  if (typeof value === "function") {
    if (!context) {
      throw new CompositionSerializationError(
        `${propName} function handlers require a serialization context`,
      );
    }

    const handler = value as (state: boolean) => void;
    return {
      kind: "runtime-state-handler",
      trueId: context.registerInteractionHandler(`${handlerKey}.true`, () => handler(true)),
      falseId: context.registerInteractionHandler(`${handlerKey}.false`, () => handler(false)),
    };
  }

  if (value == null) {
    return undefined;
  }

  throw new CompositionSerializationError(
    `${propName} must be a function handler`,
  );
}

function serializeOnClick(
  value: unknown,
  context?: CompositionSerializationContext,
  handlerKey?: string,
): unknown {
  if (isWindowActionDescriptor(value)) {
    return value.action;
  }

  if (typeof value === "function") {
    if (!context) {
      throw new CompositionSerializationError(
        "onClick function handlers require a serialization context",
      );
    }
    if (!handlerKey) {
      throw new CompositionSerializationError(
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

  throw new CompositionSerializationError(
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
        throw new CompositionSerializationError(
          `function value at "${key}" is not serializable`,
        );
      }
      serialized[key] = serializeValue(nested);
    }
    return serialized;
  }

  throw new CompositionSerializationError(
    `unsupported prop value type: ${typeof value}`,
  );
}

function childNodePath(
  parentPath: string,
  child: CompositionChild,
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
