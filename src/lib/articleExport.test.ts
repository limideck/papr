import { describe, expect, it } from "vitest";
import { articleFilename, escapeHtml } from "./articleExport";
import {
  articleToDocx,
  decodeDataImageUrl,
  imagePartExtension,
  packArticleDocx,
} from "./docx";

describe("articleFilename", () => {
  it("strips path-hostile characters and adds the extension", () => {
    expect(articleFilename('Hello / World: "x"?', "docx")).toBe(
      "Hello World x.docx",
    );
  });

  it("falls back when the title is empty after sanitizing", () => {
    expect(articleFilename(":::???", "docx")).toBe("article.docx");
  });

  it("accepts an extension with or without a leading dot", () => {
    expect(articleFilename("Note", ".docx")).toBe("Note.docx");
  });
});

describe("escapeHtml", () => {
  it("escapes the usual entities", () => {
    expect(escapeHtml(`a <b> & "c"`)).toBe("a &lt;b&gt; &amp; &quot;c&quot;");
  });
});

describe("decodeDataImageUrl", () => {
  it("decodes a base64 data: image URL", () => {
    const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47]);
    const src = `data:image/png;base64,${btoa("\x89PNG")}`;
    const parsed = decodeDataImageUrl(src);
    expect(parsed?.mime).toBe("image/png");
    expect(Array.from(parsed!.bytes)).toEqual(Array.from(png));
  });

  it("rejects non-image or malformed data URLs", () => {
    expect(decodeDataImageUrl("data:text/plain;base64,YQ==")).toBeNull();
    expect(decodeDataImageUrl("https://example.com/a.png")).toBeNull();
  });
});

describe("imagePartExtension", () => {
  it("maps Word-embeddable MIME types", () => {
    expect(imagePartExtension("image/png")).toBe("png");
    expect(imagePartExtension("image/jpeg")).toBe("jpeg");
    expect(imagePartExtension("image/webp")).toBe("webp");
    expect(imagePartExtension("image/svg+xml")).toBeNull();
  });
});

describe("packArticleDocx", () => {
  it("embeds image media parts in the zip package", async () => {
    const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    const blob = packArticleDocx(
      '<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p/></w:body></w:document>',
      [
        {
          rId: "rId1",
          relTarget: "media/image1.png",
          bytes: png,
          mime: "image/png",
          ext: "png",
        },
      ],
    );
    const buf = new Uint8Array(await blob.arrayBuffer());
    const ascii = new TextDecoder("latin1").decode(buf);
    expect(ascii).toContain("word/media/image1.png");
    expect(ascii).toContain("image/png");
    expect(ascii).toContain("relationships/image");
    // PNG magic appears as a stored zip payload (byte search — latin1
    // string matching is unreliable for 0x89 across engines).
    const hasPngMagic = [...buf.keys()].some(
      (i) =>
        buf[i] === 0x89 &&
        buf[i + 1] === 0x50 &&
        buf[i + 2] === 0x4e &&
        buf[i + 3] === 0x47,
    );
    expect(hasPngMagic).toBe(true);
  });
});

describe("articleToDocx", () => {
  // Vitest runs in plain node (no DOMParser). Empty body skips the HTML walk
  // and still exercises the ZIP / OOXML packaging path.
  it("produces a zip-shaped blob with the OOXML local-file signature", async () => {
    const blob = await articleToDocx({
      title: "Hello",
      metaLines: ["Author · Feed"],
      bodyHtml: "",
    });
    expect(blob.type).toContain("wordprocessingml");
    const buf = new Uint8Array(await blob.arrayBuffer());
    // ZIP local-file header magic: PK\x03\x04
    expect(buf[0]).toBe(0x50);
    expect(buf[1]).toBe(0x4b);
    expect(buf[2]).toBe(0x03);
    expect(buf[3]).toBe(0x04);
    expect(buf.byteLength).toBeGreaterThan(100);
  });
});
