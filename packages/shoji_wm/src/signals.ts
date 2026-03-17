export interface ReadonlySignal<T> {
  readonly value: T;
  subscribe(listener: () => void): () => void;
}

export interface Signal<T> extends ReadonlySignal<T> {
  value: T;
}

interface ReactiveComputation {
  markDirty(): void;
  registerDependency(signal: BaseSignal<unknown>): void;
}

let activeComputation: ReactiveComputation | null = null;

abstract class BaseSignal<T> implements ReadonlySignal<T> {
  protected listeners = new Set<() => void>();
  protected dependents = new Set<ReactiveComputation>();

  abstract get value(): T;

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  protected trackDependency(): void {
    if (activeComputation) {
      this.dependents.add(activeComputation);
      activeComputation.registerDependency(this);
    }
  }

  protected notify(): void {
    for (const listener of this.listeners) {
      listener();
    }
    for (const dependent of this.dependents) {
      dependent.markDirty();
    }
  }

  removeDependent(computation: ReactiveComputation): void {
    this.dependents.delete(computation);
  }
}

class WritableSignal<T> extends BaseSignal<T> implements Signal<T> {
  #value: T;

  constructor(initialValue: T) {
    super();
    this.#value = initialValue;
  }

  get value(): T {
    this.trackDependency();
    return this.#value;
  }

  set value(nextValue: T) {
    if (Object.is(this.#value, nextValue)) {
      return;
    }
    this.#value = nextValue;
    this.notify();
  }
}

class ComputedSignal<T> extends BaseSignal<T> implements ReactiveComputation {
  #compute: () => T;
  #cached!: T;
  #initialized = false;
  #dirty = true;
  #dependencies = new Set<BaseSignal<unknown>>();

  constructor(compute: () => T) {
    super();
    this.#compute = compute;
  }

  get value(): T {
    this.trackDependency();
    this.recomputeIfNeeded();
    return this.#cached;
  }

  markDirty(): void {
    if (!this.#dirty) {
      this.#dirty = true;
      this.notify();
    }
  }

  registerDependency(signal: BaseSignal<unknown>): void {
    this.#dependencies.add(signal);
  }

  private recomputeIfNeeded(): void {
    if (!this.#dirty && this.#initialized) {
      return;
    }

    for (const dependency of this.#dependencies) {
      dependency.removeDependent(this);
    }
    this.#dependencies.clear();

    const previous = activeComputation;
    activeComputation = this;
    try {
      const nextValue = this.#compute();
      const changed = !this.#initialized || !Object.is(this.#cached, nextValue);
      this.#cached = nextValue;
      this.#initialized = true;
      this.#dirty = false;
      if (changed) {
        for (const listener of this.listeners) {
          listener();
        }
      }
    } finally {
      activeComputation = previous;
    }
  }
}

class EffectHandle implements ReactiveComputation {
  #effect: () => void;
  #dependencies = new Set<BaseSignal<unknown>>();
  #disposed = false;

  constructor(effect: () => void) {
    this.#effect = effect;
    this.run();
  }

  markDirty(): void {
    if (!this.#disposed) {
      this.run();
    }
  }

  registerDependency(signal: BaseSignal<unknown>): void {
    this.#dependencies.add(signal);
  }

  dispose(): void {
    this.#disposed = true;
    for (const dependency of this.#dependencies) {
      dependency.removeDependent(this);
    }
    this.#dependencies.clear();
  }

  private run(): void {
    for (const dependency of this.#dependencies) {
      dependency.removeDependent(this);
    }
    this.#dependencies.clear();

    const previous = activeComputation;
    activeComputation = this;
    try {
      this.#effect();
    } finally {
      activeComputation = previous;
    }
  }
}

export function signal<T>(initialValue: T): Signal<T> {
  return new WritableSignal(initialValue);
}

export function computed<T>(compute: () => T): ReadonlySignal<T> {
  return new ComputedSignal(compute);
}

export function effect(run: () => void): () => void {
  const handle = new EffectHandle(run);
  return () => handle.dispose();
}

export function isSignal<T>(value: unknown): value is ReadonlySignal<T> {
  return (
    typeof value === "object" &&
    value !== null &&
    "value" in value &&
    typeof (value as ReadonlySignal<T>).subscribe === "function"
  );
}

export function read<T>(value: T | ReadonlySignal<T>): T {
  return isSignal<T>(value) ? value.value : value;
}
