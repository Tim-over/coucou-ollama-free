// A tiny, safe Markdown renderer for chat answers — builds real DOM nodes (never
// innerHTML), so model output can't inject anything. Covers what a chat reply
// uses: headings, bold/italic, inline code, fenced code blocks with a copy
// button, bullet and numbered lists, block quotes, links (web only) and rules.

import { h } from "../views/dom";

type Inline = { text: string; bold?: boolean; italic?: boolean; code?: boolean; href?: string };

/** Splits one line into styled runs: code spans, links, then bold and italic. */
function inlines(line: string): Inline[] {
  const out: Inline[] = [];
  let rest = line;

  const pushPlain = (s: string) => {
    // Bold (**x** or __x__) and italic (*x* or _x_), applied on plain text only.
    let i = 0;
    while (i < s.length) {
      const boldOpen = s.startsWith("**", i) || s.startsWith("__", i);
      if (boldOpen) {
        const marker = s.substr(i, 2);
        const end = s.indexOf(marker, i + 2);
        if (end > i + 1) {
          out.push({ text: s.slice(i + 2, end), bold: true });
          i = end + 2;
          continue;
        }
      }
      if ((s[i] === "*" || s[i] === "_") && s[i + 1] !== s[i]) {
        const end = s.indexOf(s[i], i + 1);
        if (end > i) {
          out.push({ text: s.slice(i + 1, end), italic: true });
          i = end + 1;
          continue;
        }
      }
      // Accumulate a plain run up to the next marker.
      let j = i;
      while (j < s.length && !"*_".includes(s[j]) ) j++;
      if (j === i) j = i + 1;
      out.push({ text: s.slice(i, j) });
      i = j;
    }
  };

  while (rest.length) {
    // Inline code `...`
    const tick = rest.indexOf("`");
    // Markdown link [text](url)
    const link = rest.match(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/);
    const linkIdx = link ? rest.indexOf(link[0]) : -1;

    const next = [tick, linkIdx].filter((n) => n >= 0).sort((a, b) => a - b)[0];
    if (next === undefined) {
      pushPlain(rest);
      break;
    }
    if (next > 0) pushPlain(rest.slice(0, next));

    if (next === tick) {
      const end = rest.indexOf("`", tick + 1);
      if (end > tick) {
        out.push({ text: rest.slice(tick + 1, end), code: true });
        rest = rest.slice(end + 1);
      } else {
        pushPlain(rest.slice(tick));
        break;
      }
    } else if (link && next === linkIdx) {
      out.push({ text: link[1], href: link[2] });
      rest = rest.slice(linkIdx + link[0].length);
    }
  }
  return out;
}

function renderInlines(line: string): (HTMLElement | Text)[] {
  return inlines(line).map((run) => {
    if (run.href) {
      const a = h("a", { class: "md-link", href: run.href, target: "_blank", rel: "noreferrer", text: run.text });
      return a;
    }
    if (run.code) return h("code", { class: "md-code", text: run.text });
    if (run.bold) return h("strong", { text: run.text });
    if (run.italic) return h("em", { text: run.text });
    return document.createTextNode(run.text);
  });
}

/** Renders Markdown text into a container element. */
export function renderMarkdown(text: string, onCopy?: () => void): HTMLElement {
  const root = h("div", { class: "md" });
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  let i = 0;
  let list: HTMLElement | null = null;

  const endList = () => { list = null; };

  while (i < lines.length) {
    const line = lines[i];

    // Fenced code block
    if (line.trimStart().startsWith("```")) {
      endList();
      const fence = line.trimStart().slice(0, 3);
      const code: string[] = [];
      i++;
      while (i < lines.length && !lines[i].trimStart().startsWith(fence)) {
        code.push(lines[i]);
        i++;
      }
      i++; // closing fence
      const body = code.join("\n");
      const pre = h("pre", { class: "md-pre" }, h("code", { text: body }));
      const copy = h("button", { class: "md-copy", title: "Copy", text: "⧉" });
      copy.addEventListener("click", async (e) => {
        e.stopPropagation();
        try {
          await navigator.clipboard.writeText(body);
          copy.textContent = "✓";
          onCopy?.();
          window.setTimeout(() => (copy.textContent = "⧉"), 1200);
        } catch { /* clipboard blocked */ }
      });
      root.append(h("div", { class: "md-block" }, copy, pre));
      continue;
    }

    // Blank line
    if (line.trim() === "") { endList(); i++; continue; }

    // Heading
    const head = line.match(/^(#{1,4})\s+(.*)$/);
    if (head) {
      endList();
      const level = head[1].length;
      const el = h(`h${Math.min(level + 2, 6)}` as keyof HTMLElementTagNameMap, { class: "md-h" });
      el.append(...renderInlines(head[2]));
      root.append(el);
      i++;
      continue;
    }

    // Horizontal rule
    if (/^(\s*[-*_]){3,}\s*$/.test(line)) { endList(); root.append(h("hr", { class: "md-hr" })); i++; continue; }

    // Block quote
    if (line.trimStart().startsWith(">")) {
      endList();
      const q = h("blockquote", { class: "md-quote" });
      q.append(...renderInlines(line.replace(/^\s*>\s?/, "")));
      root.append(q);
      i++;
      continue;
    }

    // List item (-, *, + or 1.)
    const li = line.match(/^(\s*)([-*+]|\d+\.)\s+(.*)$/);
    if (li) {
      const ordered = /\d+\./.test(li[2]);
      if (!list || list.dataset.ordered !== String(ordered)) {
        list = h(ordered ? "ol" : "ul", { class: "md-list" });
        list.dataset.ordered = String(ordered);
        root.append(list);
      }
      const item = h("li", {});
      item.append(...renderInlines(li[3]));
      list.append(item);
      i++;
      continue;
    }

    // Paragraph (merge consecutive plain lines)
    endList();
    const para = h("p", { class: "md-p" });
    const buf: string[] = [line];
    i++;
    while (
      i < lines.length &&
      lines[i].trim() !== "" &&
      !lines[i].trimStart().startsWith("```") &&
      !/^(#{1,4})\s/.test(lines[i]) &&
      !/^(\s*)([-*+]|\d+\.)\s+/.test(lines[i]) &&
      !lines[i].trimStart().startsWith(">")
    ) {
      buf.push(lines[i]);
      i++;
    }
    buf.forEach((l, k) => {
      if (k > 0) para.append(h("br"));
      para.append(...renderInlines(l));
    });
    root.append(para);
  }
  return root;
}

/** Does the text contain any Markdown worth rendering? Plain replies stay plain. */
export function looksLikeMarkdown(text: string): boolean {
  return /(^|\n)\s*(#{1,4}\s|[-*+]\s|\d+\.\s|>\s|```)|\*\*|`[^`]+`|\[[^\]]+\]\(https?:/.test(text);
}
