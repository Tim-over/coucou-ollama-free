// Chat view — DOM port of PromptView / ChatBubble / TypingDotsView from
// IslandViewContent.swift.
//
// A local (Ollama) answer is streamed: `chat-chunk` events grow the last reply
// word by word. A Claude answer arrives whole. Both end the same way, with the
// final text returned by `chat_send`.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { Bridge, onEvent, type ChatChunk, type ChatContext, type DocFixProgress, type DocFixResult } from "../core/bridge";
import { Sound } from "../core/sound";
import { FR, State, engineLabel, type ChatMessage, type DroppedRef } from "../core/state";
import { quickBubble } from "../core/quick";
import { renderMarkdown, looksLikeMarkdown } from "../core/markdown";
import type { ViewHost } from "./views";

let nextId = 1;

/** Copies a reply — handy for a corrected text you want to paste back. */
function copyButton(text: string): HTMLElement {
  const b = h("button", { class: "copy-btn", title: "Copy" }, svg(ICONS.doc, 10));
  b.addEventListener("click", async (e) => {
    e.stopPropagation();
    try {
      await navigator.clipboard.writeText(text);
      b.classList.add("done");
      Sound.play("pop");
      window.setTimeout(() => b.classList.remove("done"), 1200);
    } catch {
      /* clipboard refused: nothing to do */
    }
  });
  return b;
}

/** Open / Show in folder, under a reply that produced a file. */
function outputActions(path: string): HTMLElement {
  const open = h("button", { class: "out-btn primary", text: FR ? "Ouvrir" : "Open" });
  const show = h("button", { class: "out-btn", text: FR ? "Afficher dans le dossier" : "Show in folder" });
  open.addEventListener("click", () => void Bridge.openOutput(path, false));
  show.addEventListener("click", () => void Bridge.openOutput(path, true));
  return h("div", { class: "out-row" }, open, show);
}

function docFixSummary(r: DocFixResult, tracked: boolean): string {
  const lines: string[] = [];
  // Word documents count paragraphs and carry tracked changes; plain-text and
  // code files count lines and are saved as a corrected copy.
  if (FR) {
    const unit = tracked ? "paragraphe(s)" : "ligne(s)";
    lines.push(
      r.corrections === 0
        ? `Aucune faute trouvée. Une copie a quand même été enregistrée : ${r.outputName}`
        : `${r.corrections} correction(s) sur ${r.changedParagraphs} ${unit}. Enregistré sous : ${r.outputName}`,
    );
    if (r.corrections > 0 && tracked) lines.push("Ouvre-le dans Word : chaque correction est une modification suivie signée « Mochi » (Révision → Accepter / Refuser).");
    if (r.corrections > 0 && !tracked) lines.push("C'est une copie corrigée à côté de l'original (celui-ci n'a pas été touché).");
    if (r.skippedComplex > 0) lines.push(`${r.skippedComplex} paragraphe(s) laissé(s) tel(s) quel(s) : liens, champs, images ou mise en forme mélangée.`);
    if (r.rejected > 0) lines.push(`${r.rejected} ${tracked ? "réponse(s)" : "ligne(s)"} ignorée(s) : le modèle réécrivait au lieu de corriger.`);
  } else {
    const unit = tracked ? "paragraph(s)" : "line(s)";
    lines.push(
      r.corrections === 0
        ? `No mistakes found. A copy was saved anyway: ${r.outputName}`
        : `${r.corrections} correction(s) across ${r.changedParagraphs} ${unit}. Saved as: ${r.outputName}`,
    );
    if (r.corrections > 0 && tracked) lines.push("Open it in Word: every correction is a tracked change by “Mochi” (Review → Accept / Reject).");
    if (r.corrections > 0 && !tracked) lines.push("It's a corrected copy next to the original (which was left untouched).");
    if (r.skippedComplex > 0) lines.push(`${r.skippedComplex} paragraph(s) left untouched: links, fields, images or mixed formatting.`);
    if (r.rejected > 0) lines.push(`${r.rejected} ${tracked ? "answer(s)" : "line(s)"} ignored: the model rewrote instead of correcting.`);
  }
  return lines.join("\n");
}


const isDocx = (f: DroppedRef) => /\.docx$/i.test(f.name);

// Files "Correct it" hands back as a corrected file rather than chat text:
// Word (tracked changes) plus plain-text and code (a corrected copy). PDF,
// images and .odt can't be re-emitted as an edited file, so they go to chat.
const TEXT_EXT = /\.(txt|md|markdown|csv|tsv|log|rtf|tex|py|rs|js|jsx|ts|tsx|c|h|cpp|cc|hpp|cs|java|kt|go|rb|php|swift|sh|bash|ps1|sql|html?|css|scss|sass|vue|svelte|toml|ya?ml|json|xml|ini|cfg|conf|lua|r|pl|dart|scala|clj|ex|exs|bat|ps1)$/i;
const returnsFile = (f: DroppedRef) => isDocx(f) || TEXT_EXT.test(f.name);

function bubble(message: ChatMessage, live: boolean): HTMLElement {
  if (message.role === "user") {
    return h(
      "div",
      { class: "chat-row user" },
      h("div", { class: "bubble", text: quickBubble(message.content) ?? message.label ?? message.content }),
    );
  }
  // While streaming, show plain growing text; once final, render Markdown.
  const reply = h("div", { class: live ? "reply live" : "reply" });
  if (!live && !message.output && looksLikeMarkdown(message.content)) {
    reply.append(renderMarkdown(message.content, () => Sound.play("pop")));
  } else {
    reply.textContent = message.content;
  }
  if (message.output) reply.append(outputActions(message.output));
  const row = h("div", { class: "chat-row" }, reply);
  if (!live && message.content && !message.output) row.append(copyButton(message.content));
  // Ollama timing: generation speed + token counts, under the final answer.
  if (!live && message.stats && message.stats.tokensPerSec > 0) {
    const s = message.stats;
    const parts = [`${s.tokensPerSec.toFixed(1)} tok/s`, `${s.evalCount} ${FR ? "jetons" : "tokens"}`];
    if (s.totalMs > 0) parts.push(`${(s.totalMs / 1000).toFixed(1)}s`);
    row.append(h("div", { class: "chat-stats", text: parts.join(" · ") }));
  }
  return row;
}

function typingDots(): HTMLElement {
  return h(
    "div",
    { class: "chat-row" },
    h("div", { class: "typing" }, h("i"), h("i"), h("i")),
  );
}

/** The coloured chip showing what the question is about (the dropped files). */
function contextChip(label: string, names: string[]): HTMLElement {
  const chip = h("div", { class: "chip", title: names.join("\n") }, h("i", { class: "chip-dot" }), h("span", { text: label }));
  requestAnimationFrame(() => chip.classList.add("settled"));
  return chip;
}

export function buildPrompt(onHeightChange: () => void): ViewHost {
  const chipRow = h("div", { class: "chip-row" });
  const engine = h("span", { class: "engine-tag" });
  const log = h("div", { class: "chat-log" });
  const input = h("input", {
    type: "text",
    class: "chat-input",
    placeholder: "Ask me anything…",
    spellcheck: "false",
  }) as HTMLInputElement;
  const send = h("button", { class: "send-btn", title: "Send" }, svg(ICONS.arrowUp, 11));
  const bar = h("div", { class: "chat-bar" }, input, send);

  const el = h(
    "div",
    { class: "view" },
    h(
      "div",
      { class: "card wash chat-card" },
      h("div", { class: "chat-body" }, h("div", { class: "chat-top" }, chipRow, engine), log, bar),
    ),
  );
  (el.querySelector(".card") as HTMLElement).style.setProperty("--wash", "rgba(99,102,241,0.5)");

  let sending = false;
  let renderedKey = "";
  /** Stream currently being received, and the message it grows. */
  let liveStream = 0;
  let liveMessage: ChatMessage | null = null;
  let liveEl: HTMLElement | null = null;

  void onEvent<DocFixProgress>("docfix-progress", (p) => {
    if (p.streamId !== liveStream || !liveMessage) return;
    const pct = p.total ? Math.round((p.done / p.total) * 100) : 0;
    liveMessage.content = FR
      ? `Correction de ${p.name}… ${pct} % (${p.done}/${p.total})`
      : `Correcting ${p.name}… ${pct} % (${p.done}/${p.total})`;
    if (liveEl) liveEl.textContent = liveMessage.content;
  });

  void onEvent<ChatChunk>("chat-chunk", (chunk) => {
    if (chunk.streamId !== liveStream) return; // a stale answer from a reset chat
    if (!liveMessage) {
      liveMessage = { id: nextId++, role: "assistant", content: "" };
      State.chatHistory.push(liveMessage);
      State.stateOverride = "working";
      State.notify();
      onHeightChange();
    }
    liveMessage.content += chunk.text;
    // Grow the live bubble in place: re-rendering the whole log per token
    // would lose the user's text selection and scroll position.
    if (liveEl) {
      const nearBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 40;
      liveEl.textContent = liveMessage.content;
      if (nearBottom) log.scrollTop = log.scrollHeight;
    }
  });

  /**
   * "Correct it" with Word documents in the drop: each .docx gets a corrected
   * copy with tracked changes; the other files (if any) are corrected in the
   * chat as usual.
   */
  async function correctToFiles(docs: DroppedRef[]): Promise<void> {
    for (const doc of docs) {
      const streamId = Date.now();
      liveStream = streamId;
      liveMessage = { id: nextId++, role: "assistant", content: FR ? `Lecture de ${doc.name}…` : `Reading ${doc.name}…` };
      State.chatHistory.push(liveMessage);
      State.stateOverride = "working";
      renderedKey = "";
      State.notify();
      onHeightChange();
      try {
        const r = isDocx(doc)
          ? await Bridge.docFix(doc.name, doc.path, doc.source, streamId)
          : await Bridge.fileFix(doc.name, doc.path, doc.source, streamId);
        liveMessage.content = docFixSummary(r, isDocx(doc));
        liveMessage.output = r.output;
      } catch (err) {
        liveMessage.content = `${doc.name} : ${String(err).replace(/^Error:\s*/, "")}`;
      }
      liveMessage = null;
      liveStream = 0;
    }
  }

  async function submit(preset?: string, action?: string | null) {
    const query = (preset ?? input.value).trim();
    if (!query || sending) return;
    if (preset === undefined) input.value = "";

    const docs = action === "correct" ? State.droppedFiles.filter(returnsFile) : [];
    if (docs.length > 0) {
      sending = true;
      Sound.play("send");
      State.chatHistory.push({ id: nextId++, role: "user", content: query });
      const batch = State.droppedFiles.length > 1;
      await correctToFiles(docs);
      const others = State.droppedFiles.filter((f) => !returnsFile(f));
      sending = false;
      State.stateOverride = null;
      renderedKey = "";
      State.notify();
      onHeightChange();
      if (others.length === 0) {
        Sound.play("finish");
        return;
      }
      if (batch) {
        // Folder / multi-drop: the files Corriger can't turn back into a file
        // (PDF, images) are listed, not pushed into a chat one by one.
        const names = others.map((f) => f.name).join(", ");
        State.chatHistory.push({
          id: nextId++,
          role: "assistant",
          content: FR
            ? `${others.length} fichier(s) non corrigeable(s) en copie, ignoré(s) : ${names}. Glisse-les seuls pour en discuter.`
            : `${others.length} file(s) can't be corrected into a copy, skipped: ${names}. Drop them on their own to discuss them.`,
        });
        renderedKey = "";
        Sound.play("finish");
        State.notify();
        onHeightChange();
        return;
      }
      // A single non-correctable file alongside: answer about it in the chat.
      return submitChat(query, others, true);
    }
    return submitChat(query, State.droppedFiles, false);
  }

  async function submitChat(query: string, files: DroppedRef[], questionShown: boolean) {
    if (sending) return;
    sending = true;
    Sound.play("send");

    if (!questionShown) {
      const label = State.pendingLabel ?? undefined;
      State.pendingLabel = null;
      State.chatHistory.push({ id: nextId++, role: "user", content: query, label });
    }
    State.stateOverride = "thinking";
    State.notify();
    onHeightChange();

    // The files go with every turn; Rust sends each one's content only once
    // per conversation, whichever turn it first appears on. When Mochi was
    // thrown onto a window (no files), its app + title ride along as context.
    const wc = State.windowContext;
    const context: ChatContext | null = files.length
      ? { kind: "files", files: files.map((f) => ({ name: f.name, path: f.path })) }
      : wc
        ? { kind: "window", appName: wc.appName, title: wc.title, url: wc.url ?? undefined }
        : null;

    const streamId = Date.now();
    liveStream = streamId;
    liveMessage = null;

    try {
      const reply = await Bridge.chatSend(query, context, streamId);
      if (liveMessage) {
        // Streamed: the final text may differ slightly (reasoning stripped,
        // truncation note added).
        (liveMessage as ChatMessage).content = reply.text;
        if (reply.stats) (liveMessage as ChatMessage).stats = reply.stats;
      } else {
        State.chatHistory.push({ id: nextId++, role: "assistant", content: reply.text, stats: reply.stats });
      }
      // Count how much the local model generated, kept across restarts.
      if (reply.stats?.evalCount) {
        State.settings.ollamaTokensUsed = (State.settings.ollamaTokensUsed ?? 0) + reply.stats.evalCount;
        void Bridge.saveSettings(State.settings);
      }
      State.stateOverride = null;
      State.flashMood("finished");
      Sound.play("finish");
    } catch (err) {
      if (liveMessage) {
        State.chatHistory = State.chatHistory.filter((m) => m !== liveMessage);
      }
      // Drop the unanswered question too, so a retry doesn't show it twice.
      const last = State.chatHistory[State.chatHistory.length - 1];
      if (!questionShown && last?.role === "user" && last.content === query) State.chatHistory.pop();
      State.stateOverride = null;
      State.noteMessage = String(err).replace(/^Error:\s*/, "");
      State.noteActions = null;
      State.view = "note";
      Sound.play("error");
    } finally {
      liveStream = 0;
      liveMessage = null;
      sending = false;
      renderedKey = ""; // final render with copy buttons
      State.notify();
      onHeightChange();
      input.focus();
    }
  }

  send.addEventListener("click", () => void submit());
  input.addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") {
      e.preventDefault();
      void submit();
    }
    e.stopPropagation(); // Escape closes the island, not the chat
  });

  function sendPending() {
    const q = State.pendingQuery;
    if (!q || sending) return;
    const action = State.pendingAction;
    State.pendingQuery = null;
    State.pendingAction = null;
    void submit(q, action);
  }

  return {
    el,
    sync() {
      const wc = State.windowContext;
      const wcLabel = wc ? (wc.appName && wc.title ? `${wc.appName} — ${wc.title}` : (wc.appName || wc.title)) : "";
      const wantChip = State.droppedFiles.length ? State.droppedLabel : wcLabel;
      const names = State.droppedFiles.length ? State.droppedFiles.map((f) => f.name) : (wcLabel ? [wcLabel] : []);
      const chipKey = wantChip + "|" + names.join("|");
      if (chipRow.dataset.label !== chipKey) {
        chipRow.dataset.label = chipKey;
        clear(chipRow);
        if (wantChip) chipRow.append(contextChip(wantChip, names));
      }
      engine.textContent = engineLabel(State.settings);
      engine.classList.toggle("local", State.settings.provider === "ollama");

      const thinking = State.stateOverride === "thinking";
      const key = `${State.chatHistory.length}:${thinking}:${liveMessage ? "live" : ""}:${State.chatHistory.at(-1)?.output ?? ""}`;
      if (key !== renderedKey) {
        renderedKey = key;
        clear(log);
        liveEl = null;
        for (const m of State.chatHistory) {
          const live = m === liveMessage;
          const row = bubble(m, live);
          if (live) liveEl = row.querySelector(".reply");
          log.append(row);
        }
        if (thinking) log.append(typingDots());
        log.scrollTop = log.scrollHeight;
      }

      input.placeholder = State.chatHistory.length === 0 ? "Ask me anything…" : "Continue…";
      input.disabled = sending;
      if (State.prefill && !sending) {
        input.value = State.prefill;
        State.prefill = null;
        requestAnimationFrame(() => { input.focus(); input.select(); });
      }
      if (State.pendingQuery && State.view === "prompt") queueMicrotask(sendPending);
    },
    focus() {
      input.focus();
      input.select();
    },
  };
}
