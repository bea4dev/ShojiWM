// Types for the Node APIs these tests use. They run under `node --import tsx
// --test`, and @types/node cannot be the source: its global `process` clashes
// with the one in packages/shoji_wm/src/runtime-globals.d.ts, which every test
// reaches through the SDK index. The signatures mirror @types/node's, so a
// wrong call fails here as it would there. Add to this file when a test needs
// more. The root tsconfig excludes it, so a `node:` import in the runtime or
// the config still fails to typecheck.

declare module "node:test" {
  type TestFn = () => void | Promise<void>;
  type HookFn = () => void | Promise<void>;

  export function test(name: string, fn: TestFn): Promise<void>;
  export function beforeEach(fn: HookFn): void;
  export function afterEach(fn: HookFn): void;
}

declare module "node:assert/strict" {
  type AssertPredicate =
    | RegExp
    | (new (...args: any[]) => object)
    | ((thrown: unknown) => boolean)
    | object
    | Error;

  function assert(value: unknown, message?: string | Error): asserts value;
  namespace assert {
    function ok(value: unknown, message?: string | Error): asserts value;
    function equal<T>(
      actual: unknown,
      expected: T,
      message?: string | Error,
    ): asserts actual is T;
    function deepEqual<T>(
      actual: unknown,
      expected: T,
      message?: string | Error,
    ): asserts actual is T;
    function throws(block: () => unknown, message?: string | Error): void;
    function throws(
      block: () => unknown,
      error: AssertPredicate,
      message?: string | Error,
    ): void;
    function doesNotThrow(block: () => unknown, message?: string | Error): void;
    function rejects(
      block: (() => Promise<unknown>) | Promise<unknown>,
      message?: string | Error,
    ): Promise<void>;
    function rejects(
      block: (() => Promise<unknown>) | Promise<unknown>,
      error: AssertPredicate,
      message?: string | Error,
    ): Promise<void>;
  }
  export default assert;
}

declare function setImmediate(callback: () => void): unknown;
