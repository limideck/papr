// Minimal .docx (OOXML) writer — no third-party zip/docx dependency.
//
// A Word document is a ZIP of XML parts. We emit store-only (uncompressed)
// entries, which Word, LibreOffice, and Pages all open. Body HTML is walked
// with DOMParser and turned into simple WordprocessingML paragraphs/runs
// (headings, lists, bold/italic/links). <img> tags are embedded as media
// parts when bytes are available (data: URLs, or via resolveImage / fetch);
// otherwise they fall back to an italic alt-text placeholder.

import { imageMime } from "./imageBytes";

const enc = new TextEncoder();

/** CRC-32 (ISO 3309 / PNG / ZIP) over `data`. */
function crc32(data: Uint8Array): number {
  let c = 0xffffffff;
  for (let i = 0; i < data.length; i++) {
    c ^= data[i]!;
    for (let k = 0; k < 8; k++) {
      c = c & 1 ? (c >>> 1) ^ 0xedb88320 : c >>> 1;
    }
  }
  return (c ^ 0xffffffff) >>> 0;
}

interface ZipEntry {
  name: string;
  data: Uint8Array;
}

/** Build an uncompressed ZIP archive containing `files`. */
function zipStore(files: ZipEntry[]): Uint8Array {
  const locals: Uint8Array[] = [];
  const centrals: Uint8Array[] = [];
  let offset = 0;

  for (const file of files) {
    const nameBytes = enc.encode(file.name);
    const crc = crc32(file.data);
    const size = file.data.length;

    const local = new Uint8Array(30 + nameBytes.length + size);
    const lv = new DataView(local.buffer);
    lv.setUint32(0, 0x04034b50, true);
    lv.setUint16(8, 0, true); // store
    lv.setUint32(14, crc, true);
    lv.setUint32(18, size, true);
    lv.setUint32(22, size, true);
    lv.setUint16(26, nameBytes.length, true);
    local.set(nameBytes, 30);
    local.set(file.data, 30 + nameBytes.length);
    locals.push(local);

    const central = new Uint8Array(46 + nameBytes.length);
    const cv = new DataView(central.buffer);
    cv.setUint32(0, 0x02014b50, true);
    cv.setUint16(10, 0, true); // store
    cv.setUint32(16, crc, true);
    cv.setUint32(20, size, true);
    cv.setUint32(24, size, true);
    cv.setUint16(28, nameBytes.length, true);
    cv.setUint32(42, offset, true);
    central.set(nameBytes, 46);
    centrals.push(central);

    offset += local.length;
  }

  const centralSize = centrals.reduce((n, b) => n + b.length, 0);
  const end = new Uint8Array(22);
  const ev = new DataView(end.buffer);
  ev.setUint32(0, 0x06054b50, true);
  ev.setUint16(8, files.length, true);
  ev.setUint16(10, files.length, true);
  ev.setUint32(12, centralSize, true);
  ev.setUint32(16, offset, true);

  const total =
    locals.reduce((n, b) => n + b.length, 0) + centralSize + end.length;
  const out = new Uint8Array(total);
  let p = 0;
  for (const b of locals) {
    out.set(b, p);
    p += b.length;
  }
  for (const b of centrals) {
    out.set(b, p);
    p += b.length;
  }
  out.set(end, p);
  return out;
}

function xmlEscape(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/** Word requires `xml:space="preserve"` when a run has leading/trailing space. */
function wText(text: string): string {
  if (!text) return "";
  const esc = xmlEscape(text);
  const space = /^\s|\s$/.test(text) ? ' xml:space="preserve"' : "";
  return `<w:t${space}>${esc}</w:t>`;
}

interface RunStyle {
  bold?: boolean;
  italic?: boolean;
  link?: boolean;
}

function wRun(text: string, style: RunStyle = {}): string {
  if (!text) return "";
  const rPr: string[] = [];
  if (style.bold) rPr.push("<w:b/><w:bCs/>");
  if (style.italic) rPr.push("<w:i/><w:iCs/>");
  if (style.link) {
    rPr.push('<w:color w:val="0563C1"/>');
    rPr.push('<w:u w:val="single"/>');
  }
  const props = rPr.length ? `<w:rPr>${rPr.join("")}</w:rPr>` : "";
  return `<w:r>${props}${wText(text)}</w:r>`;
}

function wParagraph(
  runsXml: string,
  opts: { heading?: number; indent?: boolean } = {},
): string {
  const pPr: string[] = [];
  if (opts.heading != null) {
    // Outline level without relying on a styles.xml Title/Heading part.
    pPr.push(`<w:outlineLvl w:val="${Math.min(8, Math.max(0, opts.heading - 1))}"/>`);
    pPr.push('<w:spacing w:before="240" w:after="120"/>');
  } else {
    pPr.push('<w:spacing w:after="160"/>');
  }
  if (opts.indent) {
    pPr.push('<w:ind w:left="360"/>');
  }
  const props = pPr.length ? `<w:pPr>${pPr.join("")}</w:pPr>` : "";
  return `<w:p>${props}${runsXml || wRun("")}</w:p>`;
}

const BLOCK = new Set([
  "p",
  "div",
  "section",
  "article",
  "blockquote",
  "pre",
  "li",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "ul",
  "ol",
  "figure",
  "figcaption",
  "table",
  "tr",
  "thead",
  "tbody",
  "footer",
  "header",
  "main",
]);

const SKIP = new Set(["script", "style", "noscript", "svg", "iframe", "video", "audio"]);

/** OOXML relationship + media part for one embedded image. */
export interface DocxImagePart {
  rId: string;
  /** Path relative to word/ (e.g. media/image1.png). */
  relTarget: string;
  bytes: Uint8Array;
  mime: string;
  ext: string;
}

interface ImageCtx {
  parts: DocxImagePart[];
  nextDocPrId: number;
  resolveImage?: (src: string) => Promise<Uint8Array | null>;
}

const EMU_PER_PX = 9525; // 96 dpi
const MAX_CX = 5486400; // 6 inches — fits letter page with 1" margins

/** Decode a data:image/...;base64,... URL into bytes + MIME. */
export function decodeDataImageUrl(
  src: string,
): { bytes: Uint8Array; mime: string } | null {
  const m = /^data:(image\/[a-z0-9.+-]+);base64,([A-Za-z0-9+/=\s]+)$/i.exec(
    src.trim(),
  );
  if (!m) return null;
  try {
    const binary = atob(m[2]!.replace(/\s+/g, ""));
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
    return { bytes, mime: m[1]!.toLowerCase() };
  } catch {
    return null;
  }
}

/** File extension Word can package for a MIME type, or null if unsupported. */
export function imagePartExtension(mime: string): string | null {
  switch (mime.toLowerCase()) {
    case "image/jpeg":
    case "image/jpg":
      return "jpeg";
    case "image/png":
      return "png";
    case "image/gif":
      return "gif";
    case "image/webp":
      return "webp";
    default:
      return null;
  }
}

function pngPixelSize(bytes: Uint8Array): { w: number; h: number } | null {
  // signature (8) + IHDR length (4) + "IHDR" (4) + width/height (8)
  if (bytes.length < 24) return null;
  if (
    bytes[0] !== 0x89 ||
    bytes[1] !== 0x50 ||
    bytes[2] !== 0x4e ||
    bytes[3] !== 0x47
  ) {
    return null;
  }
  const w =
    ((bytes[16]! << 24) | (bytes[17]! << 16) | (bytes[18]! << 8) | bytes[19]!) >>>
    0;
  const h =
    ((bytes[20]! << 24) | (bytes[21]! << 16) | (bytes[22]! << 8) | bytes[23]!) >>>
    0;
  if (!w || !h) return null;
  return { w, h };
}

function gifPixelSize(bytes: Uint8Array): { w: number; h: number } | null {
  if (bytes.length < 10) return null;
  if (bytes[0] !== 0x47 || bytes[1] !== 0x49 || bytes[2] !== 0x46) return null;
  const w = bytes[6]! | (bytes[7]! << 8);
  const h = bytes[8]! | (bytes[9]! << 8);
  if (!w || !h) return null;
  return { w, h };
}

function imageExtent(bytes: Uint8Array, mime: string): { cx: number; cy: number } {
  const px =
    mime === "image/png"
      ? pngPixelSize(bytes)
      : mime === "image/gif"
        ? gifPixelSize(bytes)
        : null;
  const w = px?.w ?? 960;
  const h = px?.h ?? 540;
  let cx = Math.round(w * EMU_PER_PX);
  let cy = Math.round(h * EMU_PER_PX);
  if (cx > MAX_CX) {
    cy = Math.round(cy * (MAX_CX / cx));
    cx = MAX_CX;
  }
  return { cx, cy };
}

function wDrawing(rId: string, name: string, cx: number, cy: number, docPrId: number): string {
  const safe = xmlEscape(name || "image");
  return (
    "<w:r><w:drawing>" +
    '<wp:inline distT="0" distB="0" distL="0" distR="0">' +
    `<wp:extent cx="${cx}" cy="${cy}"/>` +
    `<wp:docPr id="${docPrId}" name="${safe}"/>` +
    '<wp:cNvGraphicFramePr><a:graphicFrameLocks xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" noChangeAspect="1"/></wp:cNvGraphicFramePr>' +
    '<a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">' +
    '<a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture">' +
    '<pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">' +
    `<pic:nvPicPr><pic:cNvPr id="0" name="${safe}"/><pic:cNvPicPr/></pic:nvPicPr>` +
    `<pic:blipFill><a:blip r:embed="${rId}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>` +
    "<pic:spPr>" +
    `<a:xfrm><a:off x="0" y="0"/><a:ext cx="${cx}" cy="${cy}"/></a:xfrm>` +
    '<a:prstGeom prst="rect"><a:avLst/></a:prstGeom>' +
    "</pic:spPr>" +
    "</pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"
  );
}

async function loadImageBytes(
  src: string,
  ctx: ImageCtx,
): Promise<{ bytes: Uint8Array; mime: string } | null> {
  if (!src) return null;

  if (src.startsWith("data:")) {
    const parsed = decodeDataImageUrl(src);
    if (!parsed || !parsed.bytes.length) return null;
    return { bytes: parsed.bytes, mime: imageMime(src, parsed.bytes) || parsed.mime };
  }

  if (!/^https?:\/\//i.test(src)) return null;

  let bytes: Uint8Array | null = null;
  if (ctx.resolveImage) {
    try {
      bytes = await ctx.resolveImage(src);
    } catch {
      bytes = null;
    }
  }
  if (!bytes || !bytes.length) {
    try {
      const res = await fetch(src);
      if (!res.ok) return null;
      bytes = new Uint8Array(await res.arrayBuffer());
    } catch {
      return null;
    }
  }
  if (!bytes.length) return null;
  return { bytes, mime: imageMime(src, bytes) };
}

async function embedImageRun(
  el: Element,
  ctx: ImageCtx,
): Promise<string | null> {
  const src =
    el.getAttribute("src")?.trim() ||
    el.getAttribute("data-papr-src")?.trim() ||
    "";
  const alt = el.getAttribute("alt")?.trim() || "image";
  const loaded = await loadImageBytes(src, ctx);
  if (!loaded) return null;
  const ext = imagePartExtension(loaded.mime);
  if (!ext) return null;

  const n = ctx.parts.length + 1;
  const rId = `rId${n}`;
  const relTarget = `media/image${n}.${ext}`;
  ctx.parts.push({
    rId,
    relTarget,
    bytes: loaded.bytes,
    mime: loaded.mime,
    ext,
  });
  const { cx, cy } = imageExtent(loaded.bytes, loaded.mime);
  const docPrId = ctx.nextDocPrId++;
  return wDrawing(rId, alt, cx, cy, docPrId);
}

function mergeStyle(base: RunStyle, extra: RunStyle): RunStyle {
  return { ...base, ...extra };
}

/** Flatten an inline subtree into Word runs. */
async function inlineRuns(
  node: Node,
  style: RunStyle = {},
  ctx: ImageCtx,
): Promise<string> {
  if (node.nodeType === Node.TEXT_NODE) {
    return wRun(node.textContent ?? "", style);
  }
  if (node.nodeType !== Node.ELEMENT_NODE) return "";
  const el = node as Element;
  const tag = el.tagName.toLowerCase();
  if (SKIP.has(tag)) return "";
  if (tag === "img") {
    const embedded = await embedImageRun(el, ctx);
    if (embedded) return embedded;
    const alt = el.getAttribute("alt")?.trim() || "image";
    return wRun(`[${alt}]`, { italic: true });
  }

  let next = style;
  if (tag === "strong" || tag === "b") next = mergeStyle(style, { bold: true });
  else if (tag === "em" || tag === "i") next = mergeStyle(style, { italic: true });
  else if (tag === "a") next = mergeStyle(style, { link: true });

  let out = "";
  for (const child of Array.from(el.childNodes)) {
    if (
      child.nodeType === Node.ELEMENT_NODE &&
      (child as Element).tagName.toLowerCase() === "br"
    ) {
      // Soft line break inside a paragraph.
      out += "<w:r><w:br/></w:r>";
    } else {
      out += await inlineRuns(child, next, ctx);
    }
  }

  // Append the href after link text so the destination survives paste/open.
  if (tag === "a") {
    const href = el.getAttribute("href");
    if (href && !href.startsWith("#")) {
      out += wRun(` (${href})`, { italic: true });
    }
  }
  return out;
}

function headingLevel(tag: string): number | undefined {
  const m = /^h([1-6])$/.exec(tag);
  return m ? Number(m[1]) : undefined;
}

/** Convert article HTML into WordprocessingML paragraph XML. */
async function htmlToParagraphs(html: string, ctx: ImageCtx): Promise<string> {
  if (!html.trim()) return wParagraph(wRun(""));
  const doc = new DOMParser().parseFromString(html, "text/html");
  const parts: string[] = [];

  const flushInline = async (
    nodes: Node[],
    opts: { heading?: number; indent?: boolean } = {},
  ) => {
    let runs = "";
    for (const n of nodes) runs += await inlineRuns(n, {}, ctx);
    if (runs || opts.heading != null) parts.push(wParagraph(runs, opts));
  };

  const walk = async (el: Element, indent = false) => {
    const tag = el.tagName.toLowerCase();
    if (SKIP.has(tag)) return;

    if (tag === "br") {
      parts.push(wParagraph(""));
      return;
    }

    const h = headingLevel(tag);
    if (h != null || tag === "p" || tag === "blockquote" || tag === "figcaption" || tag === "pre") {
      await flushInline(Array.from(el.childNodes), {
        heading: h,
        indent: indent || tag === "blockquote",
      });
      return;
    }

    if (tag === "li") {
      let runs = wRun("• ");
      for (const n of Array.from(el.childNodes)) {
        runs += await inlineRuns(n, {}, ctx);
      }
      parts.push(wParagraph(runs, { indent: true }));
      return;
    }

    if (tag === "tr") {
      const cells = Array.from(el.querySelectorAll(":scope > th, :scope > td"));
      const text = cells
        .map((c) => (c.textContent ?? "").replace(/\s+/g, " ").trim())
        .filter(Boolean)
        .join(" | ");
      if (text) parts.push(wParagraph(wRun(text), { indent }));
      return;
    }

    if (BLOCK.has(tag) || tag === "body") {
      // Group consecutive inline/text nodes into a paragraph; recurse into blocks.
      let inlineBuf: Node[] = [];
      const flush = async () => {
        if (inlineBuf.length) {
          await flushInline(inlineBuf, { indent });
          inlineBuf = [];
        }
      };
      for (const child of Array.from(el.childNodes)) {
        if (child.nodeType === Node.TEXT_NODE) {
          if ((child.textContent ?? "").trim()) inlineBuf.push(child);
          continue;
        }
        if (child.nodeType !== Node.ELEMENT_NODE) continue;
        const ct = (child as Element).tagName.toLowerCase();
        if (SKIP.has(ct)) continue;
        if (ct === "br") {
          await flush();
          parts.push(wParagraph(""));
          continue;
        }
        if (BLOCK.has(ct) || headingLevel(ct) != null) {
          await flush();
          await walk(child as Element, indent || ct === "blockquote");
        } else {
          inlineBuf.push(child);
        }
      }
      await flush();
      return;
    }

    // Unknown inline wrapper — treat children as inline in their own paragraph.
    await flushInline([el], { indent });
  };

  await walk(doc.body);
  return parts.length ? parts.join("") : wParagraph(wRun(""));
}

async function buildDocumentXml(
  opts: {
    title: string;
    metaLines: string[];
    bodyHtml: string;
  },
  ctx: ImageCtx,
): Promise<string> {
  const paras: string[] = [];
  paras.push(wParagraph(wRun(opts.title, { bold: true }), { heading: 1 }));
  for (const line of opts.metaLines) {
    if (line) paras.push(wParagraph(wRun(line, { italic: true })));
  }
  if (opts.metaLines.some(Boolean)) {
    paras.push(wParagraph(""));
  }
  paras.push(await htmlToParagraphs(opts.bodyHtml, ctx));
  paras.push(
    '<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr>',
  );

  return (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>' +
    '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" ' +
    'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" ' +
    'xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing">' +
    `<w:body>${paras.join("")}</w:body>` +
    "</w:document>"
  );
}

function buildContentTypes(images: DocxImagePart[]): string {
  const defaults = new Map<string, string>([
    ["rels", "application/vnd.openxmlformats-package.relationships+xml"],
    ["xml", "application/xml"],
  ]);
  for (const img of images) {
    defaults.set(img.ext, img.mime);
  }
  const defaultXml = [...defaults.entries()]
    .map(
      ([ext, type]) =>
        `<Default Extension="${xmlEscape(ext)}" ContentType="${xmlEscape(type)}"/>`,
    )
    .join("");
  return (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>' +
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">' +
    defaultXml +
    '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>' +
    "</Types>"
  );
}

const ROOT_RELS =
  '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>' +
  '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">' +
  '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>' +
  "</Relationships>";

function buildDocRels(images: DocxImagePart[]): string {
  const rels = images
    .map(
      (img) =>
        `<Relationship Id="${xmlEscape(img.rId)}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="${xmlEscape(img.relTarget)}"/>`,
    )
    .join("");
  return (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>' +
    '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">' +
    rels +
    "</Relationships>"
  );
}

/** Pack document XML + optional image parts into a .docx Blob (exported for tests). */
export function packArticleDocx(
  documentXml: string,
  images: DocxImagePart[] = [],
): Blob {
  const files: ZipEntry[] = [
    { name: "[Content_Types].xml", data: enc.encode(buildContentTypes(images)) },
    { name: "_rels/.rels", data: enc.encode(ROOT_RELS) },
    { name: "word/document.xml", data: enc.encode(documentXml) },
    { name: "word/_rels/document.xml.rels", data: enc.encode(buildDocRels(images)) },
  ];
  for (const img of images) {
    files.push({ name: `word/${img.relTarget}`, data: img.bytes });
  }
  const bytes = zipStore(files);
  return new Blob([bytes], {
    type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  });
}

/** Build a .docx Blob for an article (title + meta lines + HTML body).
 *
 *  Images are embedded when their bytes can be obtained from a data: URL,
 *  `resolveImage` (preferred for hotlink-protected hosts), or a plain fetch.
 *  Unresolvable / unsupported images become `[alt]` placeholders. */
export async function articleToDocx(opts: {
  title: string;
  metaLines: string[];
  bodyHtml: string;
  /** Fetch raw image bytes for an http(s) URL. Return null to skip embedding. */
  resolveImage?: (src: string) => Promise<Uint8Array | null>;
}): Promise<Blob> {
  const ctx: ImageCtx = {
    parts: [],
    nextDocPrId: 1,
    resolveImage: opts.resolveImage,
  };
  const documentXml = await buildDocumentXml(opts, ctx);
  return packArticleDocx(documentXml, ctx.parts);
}
