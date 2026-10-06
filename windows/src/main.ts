// Entry point: boot the bridge, wire the island, start the greeting.

import "./style.css";
import { Bridge, IS_TAURI, onEvent } from "./core/bridge";
import { Sound } from "./core/sound";
import { FR, State, type Settings } from "./core/state";
import type { ClipboardEvent, QuickFixEvent, ThrowResult } from "./core/bridge";
import { Island } from "./island/island";
import { registerHookHandlers } from "./island/hooks";
import { registerIntegrationHandlers, refreshConfigured } from "./island/integrations";

async function main() {
  const root = document.getElementById("root");
  if (!root) return;

  void Sound.preload();

  const island = new Island(root);

  const boot = await Bridge.boot();
  if (boot) {
    State.settings = { ...State.settings, ...boot.settings };
  }
  island.applySettings();
  State.loadIntegrationTasks();

  await onEvent<{ x: number; y: number }>("cursor", ({ x, y }) => island.onCursor(x, y));

  /** Pause has to reach Rust too, or the pollers keep calling out. */
  const setPaused = (on: boolean) => {
    if (State.paused === on) return;
    State.paused = on;
    void Bridge.setPaused(on);
  };

  await onEvent<string>("tray", (what) => {
    switch (what) {
      case "settings":
        setPaused(false);
        island.alert("settings");
        break;
      case "open":
        setPaused(false);
        island.alert(State.defaultView());
        break;
      case "pause":
        setPaused(!State.paused);
        if (State.paused) island.fsm.forceHidden();
        else island.reveal();
        break;
    }
  });

  await onEvent<null>("screen-changed", () => void Bridge.reposition());

  // The settings window writes preferences; apply them here without a restart.
  await onEvent<Settings>("settings-changed", (s) => {
    State.settings = { ...State.settings, ...s };
    island.applySettings();
    State.loadIntegrationTasks();
    void refreshConfigured();
  });

  // Global shortcut "correct the selection": Rust does the copy / fix / paste,
  // the island only shows Mochi at work and the outcome.
  let quickfixTimer = 0;
  const QF_TEXT: Record<Exclude<QuickFixEvent["phase"], "error">, (n: number) => string> = FR
    ? {
        working: () => "✦ Correction de la sélection…",
        nothing: () => "Sélectionne d'abord le texte à corriger, puis refais le raccourci.",
        toolong: () => "Sélection trop longue — glisse plutôt le fichier sur Mochi.",
        nochange: () => "Aucune faute ✓",
        done: (n) => (n === 1 ? "1 correction ✓" : `${n} corrections ✓`),
        rewrite: () => "La réponse ne ressemblait pas à une correction : rien n'a été collé (elle est dans le presse-papiers).",
        focus: () => "Tu as changé de fenêtre : la correction est dans le presse-papiers, fais Ctrl+V.",
      }
    : {
        working: () => "✦ Correcting the selection…",
        nothing: () => "Select the text to correct first, then press the shortcut again.",
        toolong: () => "That selection is too long — drop the file on Mochi instead.",
        nochange: () => "No mistakes ✓",
        done: (n) => (n === 1 ? "1 correction ✓" : `${n} corrections ✓`),
        rewrite: () => "The answer didn't look like a correction: nothing was pasted (it's on the clipboard).",
        focus: () => "You switched windows: the correction is on the clipboard, press Ctrl+V.",
      };
  await onEvent<QuickFixEvent>("quickfix", (e) => {
    window.clearTimeout(quickfixTimer);
    State.noteActions = null; // a plain status note, never the clipboard offer's buttons
    State.noteMessage = e.phase === "error" ? e.message : QF_TEXT[e.phase](e.corrections);
    if (e.phase === "working") {
      State.stateOverride = "thinking";
      island.alert("note");
      island.fsm.pinned = true; // a slow local model must not see it auto-close
      Sound.play("think");
      return;
    }
    State.stateOverride = null;
    island.fsm.pinned = false;
    if (State.view !== "note") island.alert("note");
    else State.notify();
    const good = e.phase === "done" || e.phase === "nochange";
    Sound.play(good ? "finish" : e.phase === "error" ? "error" : "question");
    quickfixTimer = window.setTimeout(() => {
      if (State.view === "note") island.collapse();
    }, good ? 2200 : 5000);
  });

  await onEvent<ThrowResult>("throw-url", (r) => void island.onThrowUrl(r));

  // Copying a chunk of text or a link anywhere: Mochi peeks and offers to help.
  await onEvent<ClipboardEvent>("clipboard", (e) => island.onClipboard(e));

  registerHookHandlers(island);
  registerIntegrationHandlers(island);

  island.launch();

  // In a plain browser there is no wake strip behind the cursor: make the whole
  // page wake the island so the visuals can be checked with `npm run dev`.
  if (!IS_TAURI) {
    document.addEventListener("click", () => Sound.resume(), { once: true });
  }
}

void main();
