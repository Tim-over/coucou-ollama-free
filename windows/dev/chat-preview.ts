// Dev harness: the chat view with a multi-file drop, a corrected .docx and a
// streaming answer, for checking the layout in a plain browser. Not shipped.

import "../src/style.css";
import { buildPrompt } from "../src/views/chat";
import { State } from "../src/core/state";

State.settings = { ...State.settings, provider: "ollama", ollamaModel: "gemma3:4b" };
State.droppedFiles = [
  { name: "contrat.docx", path: "a", source: "a" },
  { name: "photo.jpg", path: "b", source: "b" },
  { name: "notes.txt", path: "c", source: "c" },
];
State.view = "prompt";
State.chatHistory = [
  { id: 1, role: "user", content: "Corrige toutes les fautes de ce fichier (orthographe, grammaire, conjugaison, ponctuation, tournures maladroites) sans changer le sens ni le ton. Donne d'abord le texte corrigé en entier, puis la liste des corrections avec une courte explication pour chacune." },
  { id: 2, role: "assistant", content: "12 correction(s) dans 7 paragraphe(s) sur 31. Enregistré sous : contrat (corrigé).docx\nOuvre-le dans Word : chaque correction est une modification suivie signée « Mochi » (Révision → Accepter / Refuser).", output: "C:/x/contrat (corrigé).docx" },
  { id: 3, role: "assistant", content: "Pour notes.txt, voici le texte corrigé :\nIls sont venus hier." },
];
const view = buildPrompt(() => {});
view.el.style.display = "block";
view.el.style.opacity = "1";
document.getElementById("stage")!.append(view.el);
view.sync();
