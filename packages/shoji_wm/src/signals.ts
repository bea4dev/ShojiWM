import { trackSignalRead, trackSignalWrite } from "./runtime-hooks";

export type SignalSetter<T> = (next: T | ((current: T) => T)) => void;

export interface ReadonlySignal<T> {
  (): T;
  <U>(map: (value: T) => U): ReadonlySignal<U>;
  readonly value: T;
  subscribe(listener: () => void): () => void;
  peek(): T;
}

export interface Signal<T>
  extends ReadonlySignal<T> {
  value: T;
  set: SignalSetter<T>;
  update(map: (current: T) => T): void;
}

export type SignalTuple<T> = Signal<T> & readonly [Signal<T>, SignalSetter<T>];

interface ReactiveComputation {
  markDirty(): void;
  registerDependency(signal: BaseSignal<unknown>): void;
}

let activeComputation: ReactiveComputation | null = null;

abstract class BaseSignal<T> {
  protected listeners = new Set<() => void>();
  protected dependents = new Set<ReactiveComputation>();

  abstract get value(): T;
  abstract peek(): T;

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  protected trackDependency(): void {
    trackSignalRead(this);
    if (activeComputation) {
      this.dependents.add(activeComputation);
      activeComputation.registerDependency(this);
    }
  }

  protected notify(): void {
    trackSignalWrite(this);
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

class WritableSignal<T> extends BaseSignal<T> {
  #value: T;

  constructor(initialValue: T) {
    super();
    this.#value = initialValue;
  }

  get value(): T {
    this.trackDependency();
    return this.#value;
  }

  peek(): T {
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

  peek(): T {
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

export function signal<T>(initialValue: T): SignalTuple<T> {
  return createWritableSignalFacade(new WritableSignal(initialValue));
}

export function computed<T>(compute: () => T): ReadonlySignal<T> {
  return createReadonlySignalFacade(new ComputedSignal(compute));
}

export function effect(run: () => void): () => void {
  const handle = new EffectHandle(run);
  return () => handle.dispose();
}

export function isSignal<T>(value: unknown): value is ReadonlySignal<T> {
  return (
    (typeof value === "function" || typeof value === "object") &&
    value !== null &&
    "value" in value &&
    typeof (value as ReadonlySignal<T>).subscribe === "function"
  );
}

export function read<T>(value: T | ReadonlySignal<T>): T {
  return isSignal<T>(value) ? value.value : value;
}

function createReadonlySignalFacade<T>(
  source: BaseSignal<T>,
): ReadonlySignal<T> {
  const facade = ((map?: unknown) => {
    if (typeof map === "function") {
      return computed(() => (map as (value: T) => unknown)(source.value));
    }
    return source.value;
  }) as ReadonlySignal<T>;

  Object.defineProperty(facade, "value", {
    get() {
      return source.value;
    },
    enumerable: true,
    configurable: true,
  });

  facade.subscribe = source.subscribe.bind(source);
  facade.peek = source.peek.bind(source);

  return facade;
}

function createWritableSignalFacade<T>(
  source: WritableSignal<T>,
): SignalTuple<T> {
  const facade = createReadonlySignalFacade(source) as SignalTuple<T>;

  Object.defineProperty(facade, "value", {
    get() {
      return source.value;
    },
    set(nextValue: T) {
      source.value = nextValue;
    },
    enumerable: true,
    configurable: true,
  });

  const set: SignalSetter<T> = (next) => {
    source.value =
      typeof next === "function"
        ? (next as (current: T) => T)(source.peek())
        : next;
  };

  facade.set = set;
  facade.update = (map) => {
    source.value = map(source.peek());
  };
  Object.defineProperty(facade, 0, {
    value: facade,
    enumerable: false,
  });
  Object.defineProperty(facade, 1, {
    value: set,
    enumerable: false,
  });
  Object.defineProperty(facade, "length", {
    value: 2,
    enumerable: false,
  });
  facade[Symbol.iterator] = function* iterator(): IterableIterator<Signal<T> | SignalSetter<T>> {
    yield facade;
    yield set;
  };

  return facade;
}
