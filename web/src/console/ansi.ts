// SPDX-License-Identifier: AGPL-3.0-or-later

// A workload's output, rendered without letting it do anything (§8 of docs/11-surface-web.md).
// Of the terminal's escape sequences, only the colours and weights of SGR are kept, and as class
// names from a fixed set, never as markup or a style the output wrote; every other sequence (a
// cursor move, a title, a hyperlink, a clipboard write) and every control character but a line
// break and a tab is dropped. The text is text: React escapes it as it renders it.

/** A run of text and the classes of its style. */
export interface Run {
  readonly text: string;
  readonly classes: string;
}

interface Style {
  fg?: number | undefined;
  bg?: number | undefined;
  bold?: boolean;
  dim?: boolean;
  italic?: boolean;
  underline?: boolean;
}

const ESC = "\u001b";

function classesOf(s: Style): string {
  const c: string[] = [];
  if (s.fg !== undefined) c.push(`ansi-fg-${s.fg.toString()}`);
  if (s.bg !== undefined) c.push(`ansi-bg-${s.bg.toString()}`);
  if (s.bold === true) c.push("ansi-bold");
  if (s.dim === true) c.push("ansi-dim");
  if (s.italic === true) c.push("ansi-italic");
  if (s.underline === true) c.push("ansi-underline");
  return c.join(" ");
}

/** The style after the SGR parameters `params`. Extended colours (38 and 48) are skipped
 * whole, parameters included, so their numbers are not read as other attributes. */
function apply(style: Style, params: number[]): Style {
  const s: Style = { ...style };
  for (let i = 0; i < params.length; i++) {
    const p = params[i] ?? 0;
    if (p === 0)
      Object.assign(s, {
        fg: undefined,
        bg: undefined,
        bold: false,
        dim: false,
        italic: false,
        underline: false,
      });
    else if (p === 1) s.bold = true;
    else if (p === 2) s.dim = true;
    else if (p === 3) s.italic = true;
    else if (p === 4) s.underline = true;
    else if (p === 22) Object.assign(s, { bold: false, dim: false });
    else if (p === 23) s.italic = false;
    else if (p === 24) s.underline = false;
    else if (p >= 30 && p <= 37) s.fg = p - 30;
    else if (p >= 90 && p <= 97) s.fg = p - 90 + 8;
    else if (p === 39) s.fg = undefined;
    else if (p >= 40 && p <= 47) s.bg = p - 40;
    else if (p >= 100 && p <= 107) s.bg = p - 100 + 8;
    else if (p === 49) s.bg = undefined;
    else if (p === 38 || p === 48) i += params[i + 1] === 5 ? 2 : params[i + 1] === 2 ? 4 : 1;
  }
  return s;
}

/** The length of the escape sequence at `i` of `text` (which starts with ESC), its SGR
 * parameters if it is one, and nothing else about it. */
function sequence(text: string, i: number): { readonly end: number; readonly sgr?: number[] } {
  const kind = text[i + 1];
  if (kind === "[") {
    let j = i + 2;
    while (j < text.length && !/[@-~]/.test(text[j] ?? "")) j++;
    const body = text.slice(i + 2, j);
    if (text[j] === "m" && /^[\d;]*$/.test(body)) {
      return { end: j + 1, sgr: body === "" ? [0] : body.split(";").map((n) => Number(n || "0")) };
    }
    return { end: j + 1 };
  }
  if (kind === "]" || kind === "P" || kind === "_" || kind === "^") {
    // A string sequence, ended by BEL or by ESC \.
    let j = i + 2;
    while (j < text.length && text[j] !== "\u0007" && !(text[j] === ESC && text[j + 1] === "\\"))
      j++;
    return { end: text[j] === ESC ? j + 2 : j + 1 };
  }
  return { end: Math.min(text.length, i + 2) };
}

/** `text` without the control characters a terminal would act on: all below a space but tab and
 * line feed, and delete. */
function printable(text: string): string {
  let out = "";
  for (const ch of text) {
    const c = ch.charCodeAt(0);
    if (c === 9 || c === 10 || (c >= 32 && c !== 127)) out += ch;
  }
  return out;
}

/** `text` as runs of styled text, starting from `style`; the style it ends in. */
export function runs(text: string, style: Style = {}): { runs: Run[]; style: Style } {
  const out: Run[] = [];
  let current = style;
  let from = 0;
  const push = (to: number) => {
    const piece = printable(text.slice(from, to));
    if (piece !== "") out.push({ text: piece, classes: classesOf(current) });
  };
  let i = text.indexOf(ESC);
  while (i !== -1) {
    push(i);
    const seq = sequence(text, i);
    if (seq.sgr !== undefined) current = apply(current, seq.sgr);
    from = seq.end;
    i = text.indexOf(ESC, from);
  }
  push(text.length);
  return { runs: out, style: current };
}

export type { Style };
