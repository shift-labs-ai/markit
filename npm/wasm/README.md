# @shiftlabs/markit-wasm

The [markit](https://github.com/shift-labs-ai/markit) engine compiled to
WebAssembly: convert PDF, DOCX, PPTX, XLSX, HTML, EPUB and more to markdown
in a browser, with the same output as the native `@shiftlabs/markit`.

```bash
npm install @shiftlabs/markit-wasm
```

```typescript
import init, { convert } from "@shiftlabs/markit-wasm";

await init(); // fetches markit_bg.wasm next to the module
const bytes = new Uint8Array(await file.arrayBuffer());
const { markdown, title } = convert(bytes, { extension: ".pdf" });
```

`convert` is synchronous: run it in a Web Worker to keep the page responsive.

It converts the bytes it is handed and has no network or filesystem, so it
fetches no URLs, extracts no images to a directory and takes no PDF password.
The [markit README](https://github.com/shift-labs-ai/markit#browser-webassembly)
lists the limits and the bundler notes.

## License

MIT
