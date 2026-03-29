Rewrote markit's PDF converter from scratch.

Before: plain text dump, no tables.
After: markdown tables, heading structure, diagram extraction, multi-column support.

Stack: mupdf WASM for parsing, custom raycasting for table cell detection, CTM tracking for vector coordinates.

26ms for a 9-page paper. 640ms for a 224-page datasheet. 58 tests.

Tested against Intel, NXP, Microchip, and Bitcoin whitepaper PDFs — register tables, errata tables, spec tables, two-column legal docs all convert cleanly.

github.com/Michaelliv/markit
