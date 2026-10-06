// Quick actions offered after a file drop. They are ordinary chat questions,
// queued in State.pendingQuery and sent as soon as the chat opens, so the
// answer lands in the same dialog as any other reply and can be followed up.

import { State } from "./state";

export type QuickAction = "correct" | "summarize";

import { FR } from "./state";

const PROMPTS: Record<QuickAction, { fr: string; en: string }> = {
  correct: {
    fr: "Corrige toutes les fautes de ce fichier (orthographe, grammaire, conjugaison, ponctuation, tournures maladroites) sans changer le sens ni le ton. Donne d'abord le texte corrigé en entier, puis la liste des corrections avec une courte explication pour chacune.",
    en: "Proofread this file: fix every spelling, grammar, punctuation and awkward-phrasing mistake without changing the meaning or tone. First give the full corrected text, then list each correction with a short explanation.",
  },
  summarize: {
    fr: "Résume ce fichier : l'essentiel en 3 à 5 phrases, puis les points clés, les chiffres ou dates importants et ce qu'il faut en retenir ou faire.",
    en: "Summarize this file: the gist in 3 to 5 sentences, then the key points, important figures or dates, and what to remember or do next.",
  },
};

export const QUICK_LABELS: Record<QuickAction, string> = FR
  ? { correct: "Corriger", summarize: "Résumer" }
  : { correct: "Correct it", summarize: "Summarize" };

/** Queues the action; the chat view sends it when it opens. */
export function queueQuick(action: QuickAction) {
  State.pendingQuery = FR ? PROMPTS[action].fr : PROMPTS[action].en;
  State.pendingAction = action;
}

/** Short label shown in the user's bubble instead of the long prompt. */
export function quickBubble(query: string): string | null {
  for (const key of Object.keys(PROMPTS) as QuickAction[]) {
    if (PROMPTS[key].fr === query || PROMPTS[key].en === query) return `✦ ${QUICK_LABELS[key]}`;
  }
  return null;
}

// ── Quick actions on raw text (copied, or dragged onto Mochi) ──────────────────

export type TextAction = "correct" | "translate" | "summarize";

const TEXT_PROMPTS: Record<TextAction, { fr: string; en: string }> = {
  correct: {
    fr: "Corrige les fautes du texte ci-dessous (orthographe, grammaire, conjugaison, ponctuation, tournures) sans changer le sens ni le ton. Donne d'abord le texte corrigé en entier, puis la liste des corrections.",
    en: "Fix the mistakes in the text below (spelling, grammar, punctuation, awkward phrasing) without changing its meaning or tone. First give the full corrected text, then list the changes.",
  },
  translate: {
    fr: "Traduis le texte ci-dessous : s'il est en français, traduis-le en anglais ; sinon, traduis-le en français. Garde la mise en forme. Donne seulement la traduction.",
    en: "Translate the text below: if it is in English, translate it to French; otherwise translate it to English. Keep the formatting. Give only the translation.",
  },
  summarize: {
    fr: "Résume le texte ci-dessous : l'essentiel en quelques phrases, puis les points clés.",
    en: "Summarize the text below: the gist in a few sentences, then the key points.",
  },
};

export const TEXT_LABELS: Record<TextAction, string> = FR
  ? { correct: "Corriger", translate: "Traduire", summarize: "Résumer" }
  : { correct: "Correct", translate: "Translate", summarize: "Summarize" };

/**
 * Queues a text action: the full prompt (instruction + the text) is sent, but the
 * user bubble shows only a short label so it doesn't dump the whole passage.
 */
export function queueTextAction(action: TextAction, text: string) {
  const inst = FR ? TEXT_PROMPTS[action].fr : TEXT_PROMPTS[action].en;
  State.pendingQuery = `${inst}\n\n"""\n${text}\n"""`;
  State.pendingAction = null;
  State.pendingLabel = `✦ ${TEXT_LABELS[action]}`;
}
