# ShojiWM Documentation

The documentation site for [ShojiWM](https://github.com/bea4dev/ShojiWM),
built with [Docusaurus](https://docusaurus.io/).

This is a standalone npm project with its own dependencies — it is **not** part
of the npm/Cargo workspace at the repository root.

The Node.js requirement on this page applies only to building and serving the
Docusaurus documentation. ShojiWM itself runs TypeScript through the embedded
RustyScript/Deno runtime and does not require Node.js at runtime.

## Development

```bash
cd docs
npm install
npm start
```

The dev server defaults to the English locale. To preview Japanese:

```bash
npm start -- --locale ja
```

## Build

```bash
npm run build
```

The static site is emitted to `build/`. Preview it with `npm run serve`.

## GitHub Pages

The site is configured for the repository Pages URL:

```text
https://bea4dev.github.io/ShojiWM/
```

Deployment is handled by `.github/workflows/docs.yml`. In the GitHub repository
settings, set **Pages** -> **Build and deployment** -> **Source** to
**GitHub Actions**.

## Internationalization

The site supports English (`en`, default) and Japanese (`ja`).

- UI/theme strings: `i18n/ja/**/*.json`
- Translated docs: `i18n/ja/docusaurus-plugin-content-docs/current/`

After adding or changing `<Translate>` strings, regenerate the translation
stubs with:

```bash
npm run write-translations -- --locale ja
```
