# TypeScript SSD Implementation Plan

## Goal

ShojiWM の server-side decoration (SSD) を TypeScript で記述できるようにする。

当面の目標は以下:

- TypeScript/TSX で window decoration tree を返せる
- Rust compositor core がその tree を描画できる
- pointer hit-test と action 実行ができる
- xdg-decoration と接続して、将来的に `ServerSide` を既定に切り替えられる

この段階では「完全な WM フレームワーク」ではなく、まず SSD DSL を成立させる。

## Non-Goals For Now

- フル CSS 互換
- フル React 互換
- JavaScript から Wayland/DRM を直接操作すること
- 汎用 widget toolkit 化
- animation system の完成
- text shaping の完全実装

## Target Architecture

3 層に分ける。

1. Rust compositor core
2. TypeScript runtime / DSL package
3. User config / theme / SSD definition

### 1. Rust compositor core

責務:

- Wayland protocol
- input / focus / grabs
- render backend
- xdg-decoration
- window state model
- TS runtime から返された decoration tree の layout / hit-test / render

Rust 側が持つべき概念:

- `WaylandWindowHandle`
- `DecorationTree`
- `LayoutNode`
- `RenderPrimitive`
- `HitTestResult`
- `WindowAction`

### 2. TypeScript runtime / DSL package

候補パッケージ名:

- `shoji_wm`

責務:

- JSX runtime
- component definitions
- typed style object
- signal/reactivity
- Rust に渡すための serializable tree の生成

### 3. User config / theme / SSD definition

例:

```tsx
WINDOW_MANAGER.decoration = (window: WaylandWindow) => {
  const color = window.isFocused ? "#ffff00" : "#ffffff";

  return (
    <WindowBorder
      style={{
        border: { px: 1, color },
        borderRadius: 1,
      }}
    >
      <Box direction="column">
        <Box
          direction="row"
          style={{
            height: 28,
            alignItems: "center",
            paddingX: 8,
            gap: 6,
          }}
        >
          <AppIcon icon={window.icon} />
          <Label text={window.title} />
          <Box style={{ flexGrow: 1 }} />
          <Button onClick={() => window.close()} />
        </Box>
        <Window />
      </Box>
    </WindowBorder>
  );
};
```

## Recommended Repo Layout

```text
ShojiWM/
  src/                 # Rust compositor core
  docs/
  packages/
    shoji_wm/          # TS SDK / JSX runtime / signals / types
    config/            # local SSD experiments
  package.json
  deno.json            # optional, if Deno-first
  tsconfig.json
```

`node_modules/shoji_wm` を直接編集するのではなく、`packages/shoji_wm` を workspace package として育てる。

## Core Design Decisions

### 1. JSX runtime is custom

React DOM は使わない。

TSX は独自 runtime で以下のような AST を返す:

- `Box`
- `Label`
- `Button`
- `AppIcon`
- `Window`
- `WindowBorder`

### 2. `Window` is a reserved slot

`<Window />` は client surface の表示位置を示す特殊 node とする。

制約:

- decoration tree 内に最大 1 個
- 実質必須
- 普通の component と違い children を持たない

### 3. Style is typed object first

文字列 CSS は後回し。

まずは:

```ts
style={{
  paddingX: 8,
  border: { px: 1, color: "#fff" },
  flexGrow: 1,
}}
```

を正式 API とする。

### 4. Signals are first-class

AGS 的な signal system は入れる。

最初は:

- `signal`
- `computed`
- `effect`

だけで十分。

候補:

- 自前最小実装
- `@preact/signals-core`

### 5. Rust remains authoritative

TypeScript は policy/UI layer。

Rust 側が最終的に行う:

- layout validation
- hit-test
- render primitive 化
- action 実行

## Features To Implement

以下は SSD DSL を成立させるための優先機能一覧。

### Phase 1: Minimum Viable SSD DSL

必須:

- custom JSX runtime
- `WaylandWindow` 型
- `WINDOW_MANAGER.decoration` エントリポイント
- basic node types:
  - `Box`
  - `Label`
  - `Button`
  - `Window`
  - `WindowBorder`
  - `AppIcon`
- typed style object
- Rust へ渡す node tree の表現
- Rust 側で node tree を layout できること
- Rust 側で render primitive に変換できること
- Rust 側で `Window` slot に client surface を差し込めること
- close button click handling
- titlebar click -> move
- border hit-test -> resize
- xdg-decoration と接続

### Phase 2: Practical SSD

重要:

- hover / active / focused state
- icon rendering
- text truncation
- padding / gap / alignment
- per-window conditional rendering
- XWayland / Wayland distinction
- theme tokens
- signals
- derived state / computed values

### Phase 3: Quality Layer

後から:

- animations
- shadow
- blur-like effects
- gradients
- rounded clipping
- richer button components
- window controls abstraction
- resizable split layout helpers

## Style Support Scope

最初に対応すべき style は限定する。

### Layout

必須:

- `width`
- `height`
- `minWidth`
- `minHeight`
- `maxWidth`
- `maxHeight`
- `flexGrow`
- `flexShrink`
- `direction`
- `gap`
- `justifyContent`
- `alignItems`
- `padding`
- `paddingX`
- `paddingY`
- `margin`

### Visual

必須:

- `background`
- `color`
- `opacity`
- `border`
- `borderTop`
- `borderBottom`
- `borderLeft`
- `borderRight`
- `borderRadius`

### Typography

最低限:

- `fontSize`
- `fontWeight`
- `fontFamily`
- `textAlign`
- `lineHeight`

### Interaction

最低限:

- `cursor`
- `visible`

### Explicitly Out Of Scope Initially

- selector
- nested selector
- cascade
- CSS parser
- pseudo selector syntax
- arbitrary transform matrix
- full web flexbox compatibility

## Component API Surface

最初に公開すべき component:

- `Box`
- `Label`
- `Button`
- `AppIcon`
- `Window`
- `WindowBorder`

次に追加候補:

- `Spacer`
- `CloseButton`
- `MaximizeButton`
- `MinimizeButton`
- `TitleBar`

### `Box`

責務:

- row / column layout
- spacing
- alignment
- generic container

### `Label`

責務:

- title text
- fixed text rendering

最初は:

- single line only
- ellipsis support optional

### `Button`

責務:

- click action
- hover / active visual state

最初は:

- left click only
- keyboard activation不要

### `WindowBorder`

責務:

- decoration root
- border/background policy
- optional titlebar metrics ownership

### `Window`

責務:

- client content slot

## WaylandWindow API

最低限:

- `id`
- `title`
- `appId`
- `icon`
- `isFocused`
- `isXWayland()`
- `isFloating`
- `isFullscreen`
- `isMaximized`
- `close()`
- `maximize()`
- `toggleMaximize()`
- `minimize()` optional

将来:

- workspace info
- monitor/output info
- urgency
- pid / client class

## Signals

目標:

- AGS 的に reactive だが、DSL は compositor 向けに限定

最低限 API:

```ts
const focused = signal(false);
const title = computed(() => window.title);
effect(() => console.log(title.value));
```

signal 化すべき対象:

- focus
- title
- app id
- output scale
- maximized/fullscreen
- hover / active state

## Rust Side Tasks

### Data Model

- TS から受け取る decoration AST 定義
- validation layer
- normalized layout tree
- render primitive list

### Layout

最低限必要:

- box layout
- fixed size
- flex grow
- padding / gap
- border thickness
- border radius metadata

### Rendering

最初は以下だけで十分:

- solid rect
- border rect
- text
- image/icon
- client surface anchor

描画 backend は既存 Smithay renderer 経由。
OpenGL/GLES を直接 DSL から叩かない。

### Input

必要:

- pointer hit-test
- titlebar drag
- resize edge hit-test
- button onClick
- hover / pressed state

## TypeScript Side Tasks

### Package bootstrap

- `packages/shoji_wm/package.json`
- `packages/shoji_wm/src/index.ts`
- `packages/shoji_wm/src/jsx-runtime.ts`
- `packages/shoji_wm/src/types.ts`
- `packages/shoji_wm/src/components.tsx`
- `packages/shoji_wm/src/signals.ts`

### TS config

最低限:

- `jsx: "react-jsx"`
- `jsxImportSource: "shoji_wm"`

### Serialization

node tree は Rust に渡せる必要がある。

候補:

- JSON-like plain object
- flat opcode list

最初は plain object が扱いやすい。

## Suggested Milestones

### Milestone 1

Rust 側だけで SSD を描く。

目的:

- geometry
- hit-test
- server-side decoration の基礎を固める

### Milestone 2

TS runtime を追加し、固定 decoration tree を返す。

目的:

- JSX runtime
- AST bridge

### Milestone 3

window 情報を引数にした decoration function を動かす。

目的:

- dynamic title
- focus color change
- close button

### Milestone 4

signals 導入。

目的:

- reactive updates
- AGS 的な記述感

### Milestone 5

style system を拡張。

目的:

- 実用的な theme 記述

## Immediate Next Steps

優先度順:

1. Rust 側に最小 SSD render/hit-test 実装を入れる
2. `xdg-decoration` の既定ポリシーを切り替えやすい形に保つ
3. `packages/shoji_wm` を workspace package として作る
4. custom JSX runtime を作る
5. `Box`, `Label`, `Button`, `Window`, `WindowBorder` の node 化を実装する
6. Rust <-> TS の AST bridge を作る
7. focus に応じて border color が変わる最小 SSD を TSX で動かす

## Success Criteria

この段階の成功条件:

- `WINDOW_MANAGER.decoration(window)` を TSX で書ける
- titlebar が描ける
- close button が動く
- focused window で色が変わる
- `<Window />` の位置に client surface が描ける
- xdg-decoration で今後 server-side を既定にできる状態になっている
