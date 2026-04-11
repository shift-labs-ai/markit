import { describe, expect, test } from "bun:test";
import { discoverMarkdownSource } from "./markit.js";

describe("discoverMarkdownSource", () => {
  // ── link rel="alternate" ──────────────────────────────────────────

  test("finds <link rel='alternate' type='text/markdown'> with absolute href", () => {
    const html = `<html><head><link rel="alternate" type="text/markdown" href="https://example.com/post.md"></head></html>`;
    expect(discoverMarkdownSource(html, "https://example.com/post", "")).toBe(
      "https://example.com/post.md",
    );
  });

  test("finds <link> with attributes in reverse order (type before rel)", () => {
    const html = `<link type="text/markdown" rel="alternate" href="/docs/page.md">`;
    expect(
      discoverMarkdownSource(html, "https://example.com/docs/page", ""),
    ).toBe("https://example.com/docs/page.md");
  });

  test("resolves relative href against the page URL", () => {
    const html = `<link rel="alternate" type="text/markdown" href="./page.md">`;
    expect(
      discoverMarkdownSource(html, "https://example.com/docs/page", ""),
    ).toBe("https://example.com/docs/page.md");
  });

  test("resolves root-relative href", () => {
    const html = `<link rel="alternate" type="text/markdown" href="/blog/post.md">`;
    expect(
      discoverMarkdownSource(html, "https://example.com/blog/post", ""),
    ).toBe("https://example.com/blog/post.md");
  });

  test("handles single quotes in link tag", () => {
    const html = `<link rel='alternate' type='text/markdown' href='/page.md'>`;
    expect(
      discoverMarkdownSource(html, "https://example.com/page", ""),
    ).toBe("https://example.com/page.md");
  });

  test("link alternate takes priority over VitePress markers", () => {
    const html = `<head><link rel="alternate" type="text/markdown" href="/custom-source.md"></head><div id="VPContent">vitepress</div>`;
    expect(
      discoverMarkdownSource(html, "https://example.com/docs/page", ""),
    ).toBe("https://example.com/custom-source.md");
  });

  test("ignores link alternate with wrong type", () => {
    const html = `<link rel="alternate" type="application/rss+xml" href="/feed.xml">`;
    expect(
      discoverMarkdownSource(html, "https://example.com/page", ""),
    ).toBeNull();
  });

  test("ignores link alternate with empty href", () => {
    const html = `<link rel="alternate" type="text/markdown" href="">`;
    // empty href doesn't match — regex requires at least one char
    expect(
      discoverMarkdownSource(html, "https://example.com/page", ""),
    ).toBeNull();
  });

  // ── VitePress detection ───────────────────────────────────────────

  test("detects VitePress via __VP_HASH_MAP__", () => {
    const html = `<script>window.__VP_HASH_MAP__=JSON.parse("{}")</script>`;
    expect(
      discoverMarkdownSource(html, "https://docs.example.com/guide/intro", ""),
    ).toBe("https://docs.example.com/guide/intro.md");
  });

  test("detects VitePress via VPContent", () => {
    const html = `<div id="VPContent"><main>...</main></div>`;
    expect(
      discoverMarkdownSource(html, "https://docs.example.com/guide/intro", ""),
    ).toBe("https://docs.example.com/guide/intro.md");
  });

  test("detects VitePress via 'vitepress' string in HTML", () => {
    const html = `<meta name="generator" content="vitepress">`;
    expect(
      discoverMarkdownSource(html, "https://docs.example.com/api/config", ""),
    ).toBe("https://docs.example.com/api/config.md");
  });

  test("strips trailing slash before appending .md for VitePress", () => {
    const html = `<div id="VPContent"></div>`;
    expect(
      discoverMarkdownSource(
        html,
        "https://docs.example.com/guide/intro/",
        "",
      ),
    ).toBe("https://docs.example.com/guide/intro.md");
  });

  test("does NOT detect VitePress when URL has an extension", () => {
    const html = `<div id="VPContent"></div>`;
    expect(
      discoverMarkdownSource(
        html,
        "https://example.com/page.html",
        ".html",
      ),
    ).toBeNull();
  });

  // ── llms.txt convention ───────────────────────────────────────────

  test("detects llms.txt hint in HTML", () => {
    const html = `<a href="/llms.txt">LLM Context</a>`;
    expect(
      discoverMarkdownSource(html, "https://example.com/docs/page", ""),
    ).toBe("https://example.com/docs/page.md");
  });

  test("strips trailing slash before appending .md for llms.txt", () => {
    const html = `<a href="/llms.txt">llms.txt</a>`;
    expect(
      discoverMarkdownSource(html, "https://example.com/docs/page/", ""),
    ).toBe("https://example.com/docs/page.md");
  });

  test("does NOT trigger llms.txt when URL has an extension", () => {
    const html = `<a href="/llms.txt">LLM</a>`;
    expect(
      discoverMarkdownSource(html, "https://example.com/page.html", ".html"),
    ).toBeNull();
  });

  // ── No match ──────────────────────────────────────────────────────

  test("returns null for plain HTML with no markers", () => {
    const html = `<html><body><h1>Hello</h1></body></html>`;
    expect(
      discoverMarkdownSource(html, "https://example.com/page", ""),
    ).toBeNull();
  });

  test("returns null for empty HTML", () => {
    expect(
      discoverMarkdownSource("", "https://example.com/page", ""),
    ).toBeNull();
  });

  test("returns null when URL has extension even with no markers", () => {
    const html = `<html><body>plain</body></html>`;
    expect(
      discoverMarkdownSource(html, "https://example.com/file.pdf", ".pdf"),
    ).toBeNull();
  });

  // ── Edge cases ────────────────────────────────────────────────────

  test("handles URL with query string (VitePress)", () => {
    const html = `<div id="VPContent"></div>`;
    expect(
      discoverMarkdownSource(
        html,
        "https://docs.example.com/guide/intro?ref=nav",
        "",
      ),
    ).toBe("https://docs.example.com/guide/intro?ref=nav.md");
  });

  test("handles URL with hash fragment (VitePress)", () => {
    const html = `<div id="VPContent"></div>`;
    // extname won't pick up fragment, so ext is ""
    expect(
      discoverMarkdownSource(
        html,
        "https://docs.example.com/guide/intro#section",
        "",
      ),
    ).toBe("https://docs.example.com/guide/intro#section.md");
  });

  test("VitePress marker buried deep in large HTML still matches", () => {
    const padding = "<div>content</div>".repeat(500);
    const html = `<html>${padding}<script>window.__VP_HASH_MAP__={}</script></html>`;
    expect(
      discoverMarkdownSource(html, "https://example.com/docs/big-page", ""),
    ).toBe("https://example.com/docs/big-page.md");
  });

  test("multiple link alternates — first text/markdown wins", () => {
    const html = `
      <link rel="alternate" type="application/rss+xml" href="/feed.xml">
      <link rel="alternate" type="text/markdown" href="/first.md">
      <link rel="alternate" type="text/markdown" href="/second.md">
    `;
    expect(
      discoverMarkdownSource(html, "https://example.com/page", ""),
    ).toBe("https://example.com/first.md");
  });

  test("case-insensitive matching for link tag", () => {
    const html = `<LINK REL="alternate" TYPE="text/markdown" HREF="/page.md">`;
    expect(
      discoverMarkdownSource(html, "https://example.com/page", ""),
    ).toBe("https://example.com/page.md");
  });
});
