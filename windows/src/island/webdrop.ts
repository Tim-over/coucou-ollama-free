// File drops through WebView2's own drop handling (see src-tauri/src/webdrop.rs).
//
// The page accepts the drag like any web page. On drop, the File objects go to
// Rust with chrome.webview.postMessageWithAdditionalObjects; WebView2 turns
// them into real paths (folders included) and Rust answers with a
// `files-dropped` event. If that never comes back — an old runtime without the
// API — each file's bytes are sent instead.

import { onEvent } from "../core/bridge";

export type WebDropEvent =
  | { type: "enter" | "over" | "leave" }
  | { type: "drop"; paths: string[] }
  | { type: "drop-bytes"; files: File[] }
  | { type: "drop-text"; text: string };

interface WebViewBridge {
  postMessageWithAdditionalObjects?: (message: unknown, objects: ArrayLike<unknown>) => void;
}

const DROP_MESSAGE = "__coucou_drop__";
/** How long to wait for Rust's paths before falling back to bytes. */
const PATHS_TIMEOUT_MS = 1500;

function webview(): WebViewBridge | undefined {
  return (window as unknown as { chrome?: { webview?: WebViewBridge } }).chrome?.webview;
}

function hasFiles(e: DragEvent): boolean {
  const types = e.dataTransfer?.types;
  return !!types && Array.from(types).includes("Files");
}

/** A text selection being dragged (no files): text/plain in the data transfer. */
function hasText(e: DragEvent): boolean {
  const types = e.dataTransfer?.types;
  if (!types) return false;
  const list = Array.from(types);
  return !list.includes("Files") && (list.includes("text/plain") || list.includes("text"));
}

export function installWebDrop(handler: (e: WebDropEvent) => void) {
  // dragenter/dragleave fire for every child element crossed; count them.
  let depth = 0;
  let waiting: { timer: number; files: File[] } | null = null;

  void onEvent<string[]>("files-dropped", (paths) => {
    if (!waiting) return;
    window.clearTimeout(waiting.timer);
    const files = waiting.files;
    waiting = null;
    if (paths.length > 0) handler({ type: "drop", paths });
    else handler({ type: "drop-bytes", files });
  });

  window.addEventListener("dragenter", (e) => {
    if (!hasFiles(e) && !hasText(e)) return;
    e.preventDefault();
    depth += 1;
    if (depth === 1) handler({ type: "enter" });
  });

  window.addEventListener("dragover", (e) => {
    if (!hasFiles(e) && !hasText(e)) return;
    // Without preventDefault the browser shows "no drop" and never fires drop.
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
    handler({ type: "over" });
  });

  window.addEventListener("dragleave", (e) => {
    if (!hasFiles(e) && !hasText(e)) return;
    depth = Math.max(0, depth - 1);
    if (depth === 0) handler({ type: "leave" });
  });

  window.addEventListener("drop", (e) => {
    // A dragged text selection (no files): hand the text straight over.
    if (!hasFiles(e) && hasText(e)) {
      e.preventDefault();
      depth = 0;
      const text = e.dataTransfer?.getData("text/plain") || e.dataTransfer?.getData("text") || "";
      if (text.trim()) handler({ type: "drop-text", text });
      else handler({ type: "leave" });
      return;
    }
    if (!hasFiles(e)) return;
    e.preventDefault();
    depth = 0;
    const files = Array.from(e.dataTransfer?.files ?? []);
    if (files.length === 0) {
      handler({ type: "leave" });
      return;
    }
    const wv = webview();
    if (wv?.postMessageWithAdditionalObjects) {
      try {
        wv.postMessageWithAdditionalObjects(DROP_MESSAGE, e.dataTransfer!.files);
        waiting = {
          files,
          timer: window.setTimeout(() => {
            waiting = null;
            handler({ type: "drop-bytes", files });
          }, PATHS_TIMEOUT_MS),
        };
        return;
      } catch {
        /* fall through to bytes */
      }
    }
    handler({ type: "drop-bytes", files });
  });
}
