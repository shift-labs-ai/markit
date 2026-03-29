import { afterEach, describe, expect, mock, test } from "bun:test";
import { GitHubConverter } from "./github.js";

const converter = new GitHubConverter();
const originalFetch = globalThis.fetch;

afterEach(() => {
  globalThis.fetch = originalFetch;
  mock.restore();
});

// ---------------------------------------------------------------------------
// accepts
// ---------------------------------------------------------------------------

describe("accepts", () => {
  test("matches github.com URLs", () => {
    expect(converter.accepts({ url: "https://github.com/owner/repo" })).toBe(
      true,
    );
  });

  test("matches gist.github.com URLs", () => {
    expect(
      converter.accepts({ url: "https://gist.github.com/owner/abc123" }),
    ).toBe(true);
  });

  test("rejects non-GitHub URLs", () => {
    expect(converter.accepts({ url: "https://example.com" })).toBe(false);
  });

  test("rejects when no URL", () => {
    expect(converter.accepts({ extension: ".md" })).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// convertUrl — repo README
// ---------------------------------------------------------------------------

describe("repo README", () => {
  test("fetches raw README from raw.githubusercontent.com", async () => {
    const fetchMock = mock(async (input: string | URL | Request) => {
      const url = typeof input === "string" ? input : String(input);
      expect(url).toBe(
        "https://raw.githubusercontent.com/owner/repo/HEAD/README.md",
      );
      return new Response("# My Project\n\nSome description.", {
        status: 200,
      });
    });
    globalThis.fetch = fetchMock as typeof fetch;

    const result = await converter.convertUrl("https://github.com/owner/repo");
    expect(result.title).toBe("My Project");
    expect(result.markdown).toBe("# My Project\n\nSome description.");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});

// ---------------------------------------------------------------------------
// convertUrl — file blob
// ---------------------------------------------------------------------------

describe("file blob", () => {
  test("fetches raw file and wraps in code block", async () => {
    const fetchMock = mock(async (input: string | URL | Request) => {
      const url = typeof input === "string" ? input : String(input);
      expect(url).toBe(
        "https://raw.githubusercontent.com/owner/repo/main/src/index.ts",
      );
      return new Response('console.log("hello");', { status: 200 });
    });
    globalThis.fetch = fetchMock as typeof fetch;

    const result = await converter.convertUrl(
      "https://github.com/owner/repo/blob/main/src/index.ts",
    );
    expect(result.title).toBe("index.ts");
    expect(result.markdown).toBe(
      '# index.ts\n\n```ts\nconsole.log("hello");\n```',
    );
  });

  test("returns markdown files as-is", async () => {
    const fetchMock = mock(async () => {
      return new Response("# Docs\n\nHello world.", { status: 200 });
    });
    globalThis.fetch = fetchMock as typeof fetch;

    const result = await converter.convertUrl(
      "https://github.com/owner/repo/blob/main/docs/README.md",
    );
    expect(result.title).toBe("Docs");
    expect(result.markdown).toBe("# Docs\n\nHello world.");
  });
});

// ---------------------------------------------------------------------------
// convertUrl — gist
// ---------------------------------------------------------------------------

describe("gist", () => {
  test("fetches raw gist content", async () => {
    const fetchMock = mock(async (input: string | URL | Request) => {
      const url = typeof input === "string" ? input : String(input);
      expect(url).toBe("https://gist.githubusercontent.com/defunkt/2059/raw");
      return new Response("puts 'hello'", { status: 200 });
    });
    globalThis.fetch = fetchMock as typeof fetch;

    const result = await converter.convertUrl(
      "https://gist.github.com/defunkt/2059",
    );
    expect(result.title).toBe("gist:2059");
    expect(result.markdown).toBe("puts 'hello'");
  });
});

// ---------------------------------------------------------------------------
// convertUrl — issue / PR
// ---------------------------------------------------------------------------

describe("issue / PR", () => {
  test("fetches issue from GitHub API", async () => {
    const fetchMock = mock(async (input: string | URL | Request) => {
      const url = typeof input === "string" ? input : String(input);
      expect(url).toBe("https://api.github.com/repos/owner/repo/issues/42");
      return Response.json({
        title: "Bug report",
        body: "Something broke.",
        user: { login: "alice" },
        state: "open",
        labels: [{ name: "bug" }],
      });
    });
    globalThis.fetch = fetchMock as typeof fetch;

    const result = await converter.convertUrl(
      "https://github.com/owner/repo/issues/42",
    );
    expect(result.title).toBe("Bug report");
    expect(result.markdown).toContain("# Bug report");
    expect(result.markdown).toContain("@alice");
    expect(result.markdown).toContain("open");
    expect(result.markdown).toContain("bug");
    expect(result.markdown).toContain("Something broke.");
  });

  test("fetches PR via issues API endpoint", async () => {
    const fetchMock = mock(async (input: string | URL | Request) => {
      const url = typeof input === "string" ? input : String(input);
      expect(url).toBe("https://api.github.com/repos/owner/repo/issues/7");
      return Response.json({
        title: "Add feature",
        body: "This PR adds a feature.",
        user: { login: "bob" },
        state: "closed",
        labels: [],
      });
    });
    globalThis.fetch = fetchMock as typeof fetch;

    const result = await converter.convertUrl(
      "https://github.com/owner/repo/pull/7",
    );
    expect(result.title).toBe("Add feature");
    expect(result.markdown).toContain("@bob");
  });
});

// ---------------------------------------------------------------------------
// convertUrl — unsupported patterns
// ---------------------------------------------------------------------------

describe("unsupported patterns", () => {
  test("throws on github.com with no repo", async () => {
    expect(converter.convertUrl("https://github.com/owner")).rejects.toThrow(
      "Unsupported GitHub URL",
    );
  });

  test("throws on unrecognized subpath", async () => {
    expect(
      converter.convertUrl("https://github.com/owner/repo/wiki"),
    ).rejects.toThrow("Unsupported GitHub URL pattern");
  });
});

// ---------------------------------------------------------------------------
// Non-GitHub URLs still go through default fetch path
// ---------------------------------------------------------------------------

describe("Markit.convertUrl fallback", () => {
  test("non-GitHub URL is not accepted by GitHubConverter", () => {
    expect(converter.accepts({ url: "https://example.com/article" })).toBe(
      false,
    );
  });
});
