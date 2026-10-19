// Anki field cleanup — a faithful port of the helpers in
// src-tauri/src/anki.rs (strip_html / decode_entities / clean_field) so a
// headless migration produces byte-identical card text to the in-app importer.

/** Drop everything between '<' and '>', keeping the text outside tags. */
export function stripHtml(s) {
  let out = "";
  let inTag = false;
  for (const ch of s) {
    if (ch === "<") inTag = true;
    else if (ch === ">") inTag = false;
    else if (!inTag) out += ch;
  }
  return out;
}

/** Decode the small entity set Anki emits (named + numeric, dec/hex). */
export function decodeEntities(s) {
  let out = "";
  let rest = s;
  for (;;) {
    const amp = rest.indexOf("&");
    if (amp < 0) {
      out += rest;
      break;
    }
    out += rest.slice(0, amp);
    rest = rest.slice(amp);
    // Real entities are short; only look for ';' within the first 12 chars.
    const semi = rest.slice(0, Math.min(12, rest.length)).indexOf(";");
    if (semi < 0) {
      out += "&";
      rest = rest.slice(1);
      continue;
    }
    const ent = rest.slice(1, semi);
    let decoded = null;
    switch (ent) {
      case "amp": decoded = "&"; break;
      case "lt": decoded = "<"; break;
      case "gt": decoded = ">"; break;
      case "quot": decoded = '"'; break;
      case "apos": decoded = "'"; break;
      case "nbsp": decoded = " "; break;
      default: {
        if (ent.startsWith("#")) {
          const num = ent.slice(1);
          const cp = /^[xX]/.test(num) ? parseInt(num.slice(1), 16) : parseInt(num, 10);
          if (Number.isFinite(cp) && cp >= 0 && cp <= 0x10ffff) decoded = String.fromCodePoint(cp);
        }
      }
    }
    if (decoded !== null) {
      out += decoded;
      rest = rest.slice(semi + 1);
    } else {
      out += "&";
      rest = rest.slice(1);
    }
  }
  return out;
}

/** Strip HTML, decode entities, collapse whitespace (Anki's <br> artifacts). */
export function cleanField(s) {
  return decodeEntities(stripHtml(s)).split(/\s+/).filter(Boolean).join(" ");
}
