// Generates the menu-bar tray icon: a monochrome RSS glyph (matching the
// app's stroke icon set) as a 44×44 RGBA PNG. Black pixels + an alpha mask,
// so macOS can render it as a template icon that adapts to light/dark.
//
//   node scripts/gen-tray-icon.mjs  →  src-tauri/icons/tray.png

import zlib from "node:zlib";
import fs from "node:fs";

const SIZE = 44;
const SS = 4; // supersampling factor for antialiasing

// ── glyph: two broadcast arcs (r=9, r=16) + a dot, centred on (4,20) in a
//    24-unit space — the exact geometry of the design's `rss` icon. ──
const ORIGIN_X = 4;
const ORIGIN_Y = 20;
const STROKE_HALF = 0.8; // 1.6-unit stroke

function inked(u, v) {
  const r = Math.hypot(u - ORIGIN_X, v - ORIGIN_Y);
  const inQuadrant = u >= ORIGIN_X - 0.4 && v <= ORIGIN_Y + 0.4;
  if (inQuadrant && (Math.abs(r - 9) < STROKE_HALF || Math.abs(r - 16) < STROKE_HALF))
    return true;
  return Math.hypot(u - 5, v - 19) < 1.55; // the dot
}

const pixels = Buffer.alloc(SIZE * SIZE * 4);
for (let y = 0; y < SIZE; y++) {
  for (let x = 0; x < SIZE; x++) {
    let hits = 0;
    for (let sy = 0; sy < SS; sy++) {
      for (let sx = 0; sx < SS; sx++) {
        const u = ((x + (sx + 0.5) / SS) / SIZE) * 24;
        const v = ((y + (sy + 0.5) / SS) / SIZE) * 24;
        if (inked(u, v)) hits++;
      }
    }
    const i = (y * SIZE + x) * 4;
    pixels[i + 3] = Math.round((hits / (SS * SS)) * 255); // RGB stays 0 (black)
  }
}

// ── minimal PNG encoder (RGBA, 8-bit, filter 0) ──
const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
function crc32(buf) {
  let c = 0xffffffff;
  for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
}

const stride = SIZE * 4 + 1;
const raw = Buffer.alloc(SIZE * stride);
for (let y = 0; y < SIZE; y++) {
  pixels.copy(raw, y * stride + 1, y * SIZE * 4, (y + 1) * SIZE * 4);
}
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 6; // colour type: RGBA

const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", zlib.deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);
fs.writeFileSync("src-tauri/icons/tray.png", png);
console.log(`wrote src-tauri/icons/tray.png (${SIZE}×${SIZE}, ${png.length} bytes)`);
