// The island: DOM shell, sizing animation, Mochi placement, mouse handling.
// Mirrors IslandRootView.swift + IslandWindowController.swift.

import { queueQuick, queueTextAction, TEXT_LABELS } from "../core/quick";
import { PhoneLink, newPairId, type Snapshot } from "../core/phonelink";
import { OUTFITS, type Outfit } from "../mochi/engine";
import { installWebDrop } from "./webdrop";
import { Tracked, Spring, clamp } from "../core/anim";
import { Bridge, IS_TAURI, onDragDrop, type DroppedFile, type ThrowResult } from "../core/bridge";
import {
  EXPANDED_CORNER, EXPANDED_W, NOTCH_W, PANEL_H, PANEL_W,
  ROUNDED_CORNER, VIEW_LAYOUTS, botGlowColor, botGlowOpacity, botPosition, chatPromptHeight,
  islandSize,
  type IslandMode, type IslandViewName,
} from "../core/layout";
import { Sound } from "../core/sound";
import { FR, State } from "../core/state";
import { BotEngine, hexToRGB } from "../mochi/engine";
import { Greeting } from "../mochi/greeting";
import { createMiniBot, pruneMiniBots, syncMiniBotStates, tickMiniBots } from "../mochi/minibots";
import { UploadCanvas } from "../upload/canvas";
import { USC, UploadSeq } from "../upload/sequence";
import { buildHeader, buildViews, type ViewActions, type ViewHost } from "../views/views";
import { h } from "../views/dom";
import { IslandStateMachine } from "./fsm";

const BOT_OVERHANG = 40;
/** Same margin as the Rust hit test (src-tauri/src/island.rs). */
const HIT_MARGIN = 14;

/** The three views the drop sequence owns; leaving them stops the engine. */
const UPLOAD_VIEWS: ReadonlySet<IslandViewName> = new Set(["upload", "uploading", "choose"]);

/** Seconds between the drop and the moment the progress bar starts filling. */
const PRE_PROGRESS = USC.T_PROG_START - USC.T_DROP;

const modeOrder = (m: IslandMode) => (m === "hidden" ? 0 : m === "compact" ? 1 : 2);

export class Island {
  readonly fsm = new IslandStateMachine();

  private root: HTMLElement;
  private islandEl!: HTMLElement;
  private clipEl!: HTMLElement;
  private contentEl!: HTMLElement;
  private viewsEl!: HTMLElement;
  private botCanvas!: HTMLCanvasElement;
  private botGlow!: HTMLElement;
  private greetingCanvas!: HTMLCanvasElement;
  private miniGrid!: HTMLElement;
  private countdown!: HTMLElement;
  private wakeStrip!: HTMLElement;

  private header!: ViewHost;
  private views!: Map<IslandViewName, ViewHost>;
  private uploadCanvas!: UploadCanvas;

  private width = new Tracked(NOTCH_W);
  private height = new Tracked(0);
  private radius = new Tracked(ROUNDED_CORNER);
  private botCx = new Spring(46);
  private botCy = new Spring(16);
  private botSize = new Spring(10);

  private engine = new BotEngine();
  private greeting = new Greeting();

  private running = false;
  private lastFrame = 0;
  private dirty = true;
  private canvasPx = 0;

  // Rust starts the window at full size so the launch greeting has room.
  private collapsed = false;
  private collapseTimer: number | null = null;
  private wasInIsland = false;
  /** Last shape handed to Rust for the click-through test. */
  private pushedRect = { x: -1, y: -1, w: -1, h: -1 };
  private homeCollapseAt: number | null = null;

  // Bot hover → love (IslandWindowController.botHoverIn)
  private botHovering = false;
  private botHoverTimer: number | null = null;
  private lastLoveTime = 0;
  private botHoverStart = { x: 0, y: 0 };

  private confusedRecovery: number | null = null;
  private prevViewBeforeConfused: IslandViewName = "overview";
  private lastSyncedView: IslandViewName | null = null;

  /** Drop sequence bookkeeping: last tick played, and whether the ✓ has fired. */
  private uploadTens = 0;
  private uploadDone = false;

  constructor(root: HTMLElement) {
    this.root = root;
    this.build();
    this.wireFsm();
    this.wireInput();
    this.engine.onDizzy = () => this.handleDizzy();
    this.greeting.onComplete = () => this.fsm.greetComplete();
    State.subscribe(() => {
      this.dirty = true;
      this.ensureRunning();
      this.schedulePhonePublish();
    });
    this.phone.onDecision = (d) => this.remoteDecide(d.requestId, d.decision);
  }

  // ── Phone link (Android companion over Firebase) ─────────────────────────────

  private phone = new PhoneLink();
  private phonePublishTimer = 0;

  /** Starts or stops the phone link from the current settings. */
  private applyPhoneLink() {
    const s = State.settings;
    if (!s.phoneLinkEnabled || !s.firebaseApiKey || !s.firebaseProjectId) {
      this.phone.stop();
      return;
    }
    // A pair id is minted the first time the link is switched on.
    if (!s.phoneLinkPairId) {
      s.phoneLinkPairId = newPairId();
      void Bridge.saveSettings(s);
    }
    // Publish once the link is actually up — start() is async (anonymous sign-in),
    // so scheduling a publish before it resolves would be skipped (not active yet).
    void this.phone
      .start({ apiKey: s.firebaseApiKey, projectId: s.firebaseProjectId, pairId: s.phoneLinkPairId })
      .then(() => { if (this.phone.active) void this.phone.publish(this.phoneSnapshot()); });
  }

  private schedulePhonePublish() {
    if (!this.phone.active) return;
    if (this.phonePublishTimer) return;
    this.phonePublishTimer = window.setTimeout(() => {
      this.phonePublishTimer = 0;
      void this.phone.publish(this.phoneSnapshot());
    }, 500) as unknown as number;
  }

  private phoneSnapshot(): Snapshot {
    const req = State.pendingApproval;
    const sessions = State.tasks.map((t) => {
      const inApproval = t.state === "approval" && !!req;
      const steps = t.steps ?? [];
      const step = steps[t.stepIndex] ?? steps[steps.length - 1] ?? "";
      return {
        pillId: t.id,
        name: t.name,
        color: t.color,
        state: t.state,
        step,
        needsApproval: inApproval,
        approvalId: inApproval ? req!.requestId : "",
        approvalCommand: inApproval ? (req!.command || req!.tool || "") : "",
      };
    });
    return { pcName: "Coucou PC", sessions, updatedAt: Date.now() };
  }

  /** Allow/deny arriving from the phone — same effect as the island's buttons. */
  private remoteDecide(requestId: string, decision: "allow" | "deny") {
    const req = State.pendingApproval;
    if (!req || req.requestId !== requestId) return; // already answered, or stale
    Sound.play(decision === "deny" ? "blip" : "approve");
    void Bridge.approvalDecision(req.requestId, decision);
    State.pendingApproval = null;
    State.isPinned = false;
    this.fsm.pinned = false;
    State.updateTask("integration_claude", "working");
    State.setPillBadge("integration_claude", null);
    if (State.view === "approval") this.setView(State.defaultView());
    State.notify();
  }

  // ── DOM ─────────────────────────────────────────────────────────────────────

  private build() {
    const actions: ViewActions = {
      setView: (v) => this.setView(v),
      collapse: () => this.collapse(),
      setFocus: (id) => {
        State.setFocus(id);
        Sound.play("blip");
      },
      openTerminal: () => {
        const cwd = State.focusTask?.sessionCwd ?? null;
        void Bridge.openInVSCode(cwd);
      },
      // The ↗ button — same targets as openAgentTarget() on macOS.
      openTarget: () => {
        const task = State.focusTask;
        if (!task) return;
        const urls: Record<string, string> = {
          integration_resend: "https://resend.com/emails",
          integration_vercel: "https://vercel.com/dashboard",
          integration_github: "https://github.com",
          integration_stripe: "https://dashboard.stripe.com/payments",
          integration_notion: "https://notion.so",
          integration_calcom: "https://app.cal.com/bookings",
        };
        if (task.id === "integration_claude") void Bridge.openInVSCode(task.sessionCwd ?? null);
        else if (task.id === "integration_n8n") void Bridge.openN8n();
        else if (urls[task.id]) void Bridge.openUrl(urls[task.id]);
      },
      openUrl: (url) => {
        if (url) void Bridge.openUrl(url);
      },
      decide: (d) => {
        const req = State.pendingApproval;
        void Bridge.log(`decide ${d} req=${req?.requestId ?? "none"}`);
        if (!req) return;
        Sound.play(d === "deny" ? "blip" : "approve");
        void Bridge.approvalDecision(req.requestId, d);
        State.pendingApproval = null;
        State.isPinned = false;
        this.fsm.pinned = false;
        State.updateTask("integration_claude", "working");
        State.setPillBadge("integration_claude", null);
        this.setView(State.defaultView());
      },
      toggleSound: () => {
        State.settings.soundEnabled = !State.settings.soundEnabled;
        Sound.setEnabled(State.settings.soundEnabled);
        void Bridge.saveSettings(State.settings);
        State.notify();
      },
      setVolume: (v) => {
        State.settings.soundVolume = v;
        Sound.setVolume(v);
        void Bridge.saveSettings(State.settings);
        State.notify();
      },
      setAutoClose: (s) => {
        State.settings.autoCloseInterval = s;
        this.fsm.homeToPetitDelay = s;
        void Bridge.saveSettings(State.settings);
        State.notify();
      },
      openSettingsWindow: () => void Bridge.openSettingsWindow(),
      blip: () => Sound.play("blip"),
    };

    this.wakeStrip = h("div", { id: "wake-strip" });
    this.botGlow = h("div", { id: "bot-glow" });
    this.botCanvas = h("canvas", { id: "bot-canvas" });
    this.greetingCanvas = h("canvas", { id: "greeting-canvas" });
    this.miniGrid = h("div", { id: "mini-grid" });
    this.countdown = h("div", { id: "countdown" });

    this.header = buildHeader(actions);
    this.views = buildViews(actions, () => this.animateGeometry(false));
    this.viewsEl = h("div", { id: "views" });
    for (const v of this.views.values()) this.viewsEl.append(v.el);
    this.contentEl = h("div", { id: "content" }, this.header.el, this.viewsEl);

    // The drop sequence draws the card, the bar and its own Mochi. It sits under
    // the header, which stays visible on top of it exactly as on macOS.
    this.uploadCanvas = new UploadCanvas({
      ask: () => {
        this.setView("prompt");
      },
      quick: (action) => {
        queueQuick(action);
        this.setView("prompt");
      },
      cancel: () => this.setView(State.defaultView()),
    });

    this.clipEl = h(
      "div",
      { id: "island-clip" },
      this.greetingCanvas,
      this.uploadCanvas.el,
      this.contentEl,
    );
    this.islandEl = h(
      "div",
      { id: "island" },
      this.clipEl,
      this.botGlow,
      this.botCanvas,
      this.miniGrid,
      this.countdown,
    );

    const dpr = Math.min(2, window.devicePixelRatio || 1);
    this.greetingCanvas.width = Math.round(EXPANDED_W * dpr);
    this.greetingCanvas.height = Math.round(150 * dpr);
    this.greetingCanvas.style.width = `${EXPANDED_W}px`;
    this.greetingCanvas.style.height = "150px";

    this.root.append(this.wakeStrip, this.islandEl);
    this.applyGeometry();
  }

  // ── FSM ─────────────────────────────────────────────────────────────────────

  private wireFsm() {
    this.fsm.homeToPetitDelay = State.settings.autoCloseInterval;
    this.fsm.onTransition = (from, to) => {
      switch (to) {
        case "hidden":
          this.setMode("hidden");
          break;
        case "petit":
          if (from === "coucou") this.greeting.interrupt();
          else if (from === "hidden") Sound.play("peek");
          this.setMode("compact");
          if (from === "coucou") State.view = State.defaultView();
          if (!this.wasInIsland) this.fsm.mouseLeft();
          break;
        case "home":
          this.expand(State.defaultView());
          if (!this.wasInIsland) this.fsm.mouseLeft();
          break;
        case "coucou":
          this.expand("greeting");
          this.greeting.start();
          break;
      }
      State.notify();
    };
  }

  launch() {
    this.fsm.launch();
  }

  // ── Mode / view ─────────────────────────────────────────────────────────────

  private setMode(mode: IslandMode) {
    const prev = State.mode;
    if (mode === prev) return;
    State.mode = mode;
    if (mode === "expanded") Sound.play("open");
    if (prev === "expanded") {
      Sound.play("close");
      State.isPinned = false;
      void Bridge.focusWindow(false);
    }
    if (mode !== "expanded") {
      this.engine.resetMorph();
      // Nothing can be seen of the sequence once the island is shut, and leaving
      // it running would keep the frame loop awake — the island must cost
      // nothing while hidden.
      UploadSeq.deactivate();
    }
    this.updateWindowCollapsed();
    this.animateGeometry(modeOrder(mode) < modeOrder(prev));
    State.notify();
  }

  /** True while the drop sequence owns the island body. */
  private get uploadActive(): boolean {
    return State.mode === "expanded" && UploadSeq.isActive && UPLOAD_VIEWS.has(State.view);
  }

  /** Navigating out of the drop flow ends the sequence, as on macOS. */
  private stopSequenceIfLeaving(view: IslandViewName) {
    if (UploadSeq.isActive && !UPLOAD_VIEWS.has(view)) UploadSeq.deactivate();
  }

  expand(view: IslandViewName) {
    this.stopSequenceIfLeaving(view);
    State.view = view;
    if (State.mode !== "expanded") this.setMode("expanded");
    else this.animateGeometry(false);
    State.lastActivity = performance.now();
    this.homeCollapseAt = null;
    State.notify();
  }

  setView(view: IslandViewName) {
    this.stopSequenceIfLeaving(view);
    if (State.mode !== "expanded") {
      this.fsm.forceHome();
      State.view = view;
      this.animateGeometry(false);
      State.notify();
      return;
    }
    const grew = VIEW_LAYOUTS[view].height >= VIEW_LAYOUTS[State.view].height;
    State.view = view;
    State.lastActivity = performance.now();
    this.animateGeometry(!grew);
    State.notify();
  }

  collapse() {
    State.isPinned = false;
    this.fsm.pinned = false;
    // Drive the state machine rather than the mode: setting the mode behind its
    // back left it thinking the island was still open, and a click on the compact
    // island then did nothing — the island could never be reopened.
    this.fsm.forcePetit();
  }

  /** Alert from the hook server: open on this view. Pinned alerts never auto-close. */
  alert(view: IslandViewName) {
    this.fsm.pinned = State.isPinned;
    this.fsm.forceHome();
    this.expand(view);
  }

  reveal() {
    this.fsm.reveal();
  }

  /** An alert stopped waiting for an answer: let the island auto-close again. */
  dropPin() {
    this.fsm.pinned = false;
  }

  // ── File drop ───────────────────────────────────────────────────────────────

  private onDragDrop(e: { type: string; paths?: string[] }) {
    if (e.type !== "over") void Bridge.log(`drag ${e.type} ${e.paths?.length ?? 0} file(s)`);
    if (State.paused) return;
    switch (e.type) {
      case "enter":
      case "over": {
        if (State.fileDragOver) return;
        State.fileDragOver = true;
        this.engine.animateMorph(1);
        // enterZone must run before the island expands, so the sequence is
        // already active by the time the view becomes `upload`.
        UploadSeq.enterZone(State.mouseInIsland.x, State.mouseInIsland.y);
        this.alert("upload");
        break;
      }
      case "leave": {
        if (!State.fileDragOver) return;
        State.fileDragOver = false;
        this.engine.animateMorph(0);
        // The island deliberately stays open: the drag session is still alive.
        UploadSeq.exitZone();
        State.notify();
        break;
      }
      case "drop": {
        State.fileDragOver = false;
        const paths = e.paths ?? [];
        if (paths.length === 0) {
          this.engine.animateMorph(0);
          this.setView(State.defaultView());
          return;
        }
        this.swallow(paths);
        break;
      }
    }
  }

  /**
   * Mochi eats the file. Nothing here waits on the file system: the copy into
   * the inbox runs in the background and swaps the path in when it lands, so a
   * slow disk can never stall the animation — same as FileDropHandler on macOS.
   */
  private swallow(paths: string[], ingest?: () => Promise<DroppedFile[]>) {
    // Provisional names for the animation; a folder shows as its own name
    // until the walk reports what is inside.
    State.droppedFiles = paths.map((p) => ({ name: p.split(/[\\/]/).pop() || "file", path: p, source: p }));
    State.windowContext = null;
    State.promptContext = null;
    State.pendingQuery = null;
    State.pendingAction = null;
    State.chatHistory = [];
    void Bridge.chatReset();

    UploadSeq.performDrop(State.uploadDuration);
    this.uploadTens = 0;
    this.uploadDone = false;

    this.engine.gulp();
    Sound.play("approve");
    this.engine.triggerEmote("happy");
    this.engine.animateMorph(0);

    State.uploadProgress = 0;
    this.setView("uploading");
    this.ensureRunning();

    void (ingest ? ingest() : Bridge.ingestPaths(paths))
      .then((files) => {
        State.droppedFiles = files.map((f) => ({ name: f.name, path: f.path, source: f.source }));
        State.notify();
      })
      .catch((err) => {
        UploadSeq.deactivate();
        State.noteMessage = String(err).replace(/^Error:\s*/, "");
        this.engine.animateMorph(0);
        this.setView("note");
        Sound.play("error");
        window.setTimeout(() => this.setView(State.defaultView()), 2400);
      });
  }

  /**
   * Sounds and view changes hung off the canvas timeline: a `tick` every 10 %,
   * the ✓ chime when the bar completes, then `choose` once Mochi has grown back.
   */
  private stepSequence() {
    const since = UploadSeq.sinceDrop();
    if (since == null) return;
    const dur = State.uploadDuration;
    const p = Math.max(0, Math.min(1, (since - PRE_PROGRESS) / dur));

    const tens = Math.floor(p * 10);
    if (tens > this.uploadTens && tens < 10) {
      this.uploadTens = tens;
      Sound.play("tick");
    }

    if (!this.uploadDone && since >= PRE_PROGRESS + dur) {
      this.uploadDone = true;
      Sound.play("approve");
      this.engine.triggerEmote("happy");
    }
    // The extra second is the grow-back, after which the choose card is up.
    if (since >= PRE_PROGRESS + dur + 1 && State.view === "uploading") {
      this.setView("choose");
    }
  }

  // ── Geometry ────────────────────────────────────────────────────────────────

  private targetSize(): { w: number; h: number; r: number } {
    const { w, h } = islandSize(State.mode, State.view, State.chatHistory.length);
    const r = State.mode === "expanded" ? EXPANDED_CORNER : ROUNDED_CORNER;
    return { w, h, r };
  }

  private animateGeometry(shrinking: boolean) {
    const { w, h, r } = this.targetSize();
    if (shrinking) {
      this.width.curveTowards(w);
      this.height.curveTowards(h);
      this.radius.curveTowards(r);
    } else {
      this.width.springTo(w);
      this.height.springTo(h);
      this.radius.springTo(r);
    }
    this.ensureRunning();
  }

  private applyGeometry() {
    const w = this.width.value;
    const hh = this.height.value;
    const r = this.radius.value;
    this.islandEl.style.width = `${w}px`;
    this.islandEl.style.height = `${hh}px`;
    this.islandEl.style.borderRadius = `0 0 ${r}px ${r}px`;
    this.islandEl.style.transform = `translateX(-50%)`;
    // These follow the island as it resizes, so they belong here rather than in
    // the state-driven DOM sync.
    this.miniGrid.style.left = `${w - 40 - 14.5}px`;
    this.miniGrid.style.top = `${hh / 2 - 14.5}px`;
    this.greetingCanvas.style.left = `${(w - EXPANDED_W) / 2}px`;
    this.uploadCanvas.el.style.left = `${(w - EXPANDED_W) / 2}px`;

    const rect = { x: (PANEL_W - w) / 2, y: 0, w, h: hh };
    const p = this.pushedRect;
    if (Math.abs(p.x - rect.x) > 0.5 || Math.abs(p.w - rect.w) > 0.5 || Math.abs(p.h - rect.h) > 0.5) {
      this.pushedRect = rect;
      void Bridge.setIslandRect(rect.x, rect.y, rect.w, rect.h);
    }
  }

  /** Island rect in window coordinates (origin top-left of the 720×320 window). */
  private islandRect(): { x: number; y: number; w: number; h: number } {
    const w = this.width.value;
    const hh = this.height.value;
    return { x: (PANEL_W - w) / 2, y: 0, w, h: hh };
  }

  // ── Window collapse (hidden → tiny wake strip, zero polling) ────────────────

  private updateWindowCollapsed() {
    if (this.collapseTimer != null) {
      window.clearTimeout(this.collapseTimer);
      this.collapseTimer = null;
    }
    if (State.mode === "hidden") {
      // Let the island finish retracting, then drop the window to the wake strip:
      // from there the OS delivers no cursor events, so nothing polls at all.
      this.collapseTimer = window.setTimeout(() => {
        this.collapseTimer = null;
        if (State.mode !== "hidden") return;
        this.collapsed = true;
        void Bridge.setCollapsed(true);
      }, 420);
    } else if (this.collapsed) {
      // Grow the window back before the island animates open.
      this.collapsed = false;
      void Bridge.setCollapsed(false);
    }
  }

  // ── Input ───────────────────────────────────────────────────────────────────

  private wireInput() {
    // The wake strip is the only thing the OS can hit while the island is hidden.
    this.wakeStrip.addEventListener("mouseenter", () => {
      Sound.resume();
      if (State.mode === "hidden") this.fsm.mouseEntered();
    });

    this.islandEl.addEventListener("mousedown", (e) => {
      Sound.resume();
      State.lastActivity = performance.now();
      if (State.mode !== "expanded") {
        this.fsm.click();
        return;
      }
      if (this.isBotHit(e.clientX, e.clientY)) {
        this.cancelBotHover();
        this.beginBotPress(e.clientX, e.clientY);
      }
    });

    this.islandEl.addEventListener("contextmenu", (e) => {
      if (State.mode === "expanded" && this.isBotHit(e.clientX, e.clientY)) {
        e.preventDefault();
        this.toggleWardrobe();
      }
    });

    window.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && State.mode === "expanded" && !State.isPinned) this.collapse();
      State.lastActivity = performance.now();
    });

    // Drops arrive through WebView2 (webdrop.ts). The Tauri drag events stay
    // wired in case dragDropEnabled is ever turned back on.
    installWebDrop((e) => {
      if (e.type === "drop-bytes") {
        State.fileDragOver = false;
        void Bridge.log(`drag drop ${e.files.length} file(s) (bytes)`);
        this.swallow(
          e.files.map((f) => f.name),
          () => Promise.all(e.files.map((f) => Bridge.ingestBytes(f))),
        );
        return;
      }
      if (e.type === "drop-text") {
        void Bridge.log(`drag drop text (${e.text.length} chars)`);
        this.onTextDropped(e.text);
        return;
      }
      this.onDragDrop(e);
    });
    void onDragDrop((e) => this.onDragDrop(e));

    // Outside Tauri (plain browser) drive the cursor from DOM events so the
    // island can be inspected with `npm run dev`.
    if (!IS_TAURI) {
      window.addEventListener("mousemove", (e) => this.onCursor(e.clientX, e.clientY));
    }
  }

  private wardrobeEl: HTMLElement | null = null;

  /** Right-click Mochi → a small panel to dress him up. */
  private toggleWardrobe() {
    if (this.wardrobeEl) {
      this.wardrobeEl.remove();
      this.wardrobeEl = null;
      return;
    }
    const labels: Record<Outfit, string> = FR
      ? { none: "Aucun", party: "Chapeau", beanie: "Bonnet", crown: "Couronne", santa: "Père Noël", bunny: "Oreilles", bow: "Nœud", sunglasses: "Lunettes soleil", glasses: "Lunettes", scarf: "Écharpe" }
      : { none: "None", party: "Party hat", beanie: "Beanie", crown: "Crown", santa: "Santa", bunny: "Bunny ears", bow: "Bow", sunglasses: "Sunglasses", glasses: "Glasses", scarf: "Scarf" };
    const panel = h("div", { class: "wardrobe" });
    panel.append(h("div", { class: "wardrobe-title", text: FR ? "Garde-robe" : "Wardrobe" }));
    const grid = h("div", { class: "wardrobe-grid" });
    for (const o of OUTFITS) {
      const current = (State.settings.outfit ?? "none") === o;
      const b = h("button", { class: current ? "wardrobe-item on" : "wardrobe-item", text: labels[o] });
      b.addEventListener("click", () => {
        State.settings.outfit = o;
        this.engine.outfit = o;
        void Bridge.saveSettings(State.settings);
        this.ensureRunning();
        this.toggleWardrobe();
        this.engine.triggerEmote("happy");
        Sound.play("pop");
      });
      grid.append(b);
    }
    panel.append(grid);
    this.islandEl.append(panel);
    this.wardrobeEl = panel;
    // Close on Escape or a click elsewhere.
    window.setTimeout(() => {
      const close = (ev: MouseEvent) => {
        if (this.wardrobeEl && !this.wardrobeEl.contains(ev.target as Node)) {
          this.wardrobeEl.remove(); this.wardrobeEl = null;
          window.removeEventListener("mousedown", close, true);
        }
      };
      window.addEventListener("mousedown", close, true);
    }, 0);
  }

  /**
   * Mouse down on Mochi: a quick release is a slap, dragging it off the island
   * is a "throw" — on release Rust reads the URL of the window underneath.
   */
  private beginBotPress(x0: number, y0: number) {
    let thrown = false;
    const move = (ev: MouseEvent) => {
      if (thrown) return;
      if (Math.hypot(ev.clientX - x0, ev.clientY - y0) > 14) {
        thrown = true;
        cleanup();
        this.startThrow();
      }
    };
    const up = () => {
      cleanup();
      if (!thrown) this.engine.slap();
    };
    const cleanup = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  }

  private throwing = false;
  private throwPrevPinned = false;

  private startThrow() {
    if (this.throwing) return;
    this.throwing = true;
    this.throwPrevPinned = this.fsm.pinned;
    this.fsm.pinned = true; // stay open while the cursor is over the browser
    State.noteMessage = FR
      ? "Lâche Mochi sur un onglet GitHub ou une page web…"
      : "Drop Mochi on a GitHub tab or a web page…";
    State.throwing = true; // hide the island Mochi; the ghost carries it
    this.alert("note");
    this.ensureRunning();
    void Bridge.beginThrow();
  }

  /** Rust answers the throw with the window's URL (or why there is none). */
  async onThrowUrl(r: ThrowResult) {
    if (!this.throwing) return;
    this.throwing = false;
    this.fsm.pinned = this.throwPrevPinned;
    // Mochi pops back into the island.
    State.throwing = false;
    this.engine.grab();
    this.ensureRunning();

    if (r.overSelf) {
      State.stateOverride = null;
      State.noteActions = null;
      State.noteMessage = FR
        ? "Lâche Mochi sur la PAGE du navigateur (Opera GX, Chrome…), pas sur Coucou."
        : "Drop Mochi on the browser PAGE (Opera GX, Chrome…), not on Coucou.";
      this.alert("note");
      Sound.play("error");
      window.setTimeout(() => { if (State.view === "note") this.setView(State.defaultView()); }, 4500);
      return;
    }
    if (!r.url) {
      // No URL: not a browser page (or the address couldn't be read). If we at
      // least know which window it was, attach it as context — same idea as
      // dropping Mochi on a window on macOS — and open the chat.
      const appName = (r.app || "").trim();
      const title = (r.title || "").trim();
      if (appName || title) {
        State.stateOverride = null;
        State.droppedFiles = [];
        State.windowContext = { appName, title, url: null };
        State.chatHistory = [];
        State.pendingQuery = null;
        State.pendingAction = null;
        void Bridge.chatReset();
        const label = title && appName ? `${appName} — ${title}` : (appName || title);
        State.prefill = FR
          ? `Voici la fenêtre « ${label} ». Qu'est-ce que c'est et comment m'en servir ?`
          : `Here's the window "${label}". What is it and how do I use it?`;
        this.alert("prompt");
        Sound.play("finish");
        return;
      }
      State.stateOverride = null;
      State.noteActions = null;
      State.noteMessage = FR
        ? "Je n'ai pas pu lire cette fenêtre. Réessaie en visant bien la fenêtre."
        : "Couldn't read that window. Try again, aiming right at the window.";
      this.alert("note");
      Sound.play("error");
      window.setTimeout(() => { if (State.view === "note") this.setView(State.defaultView()); }, 5000);
      return;
    }

    await this.scanAndOpen(r.url);
  }

  /** Fetches a URL (GitHub repo or web page) and opens the chat about it. */
  async scanAndOpen(url: string) {
    const host = url.replace(/^https?:\/\//, "").split("/")[0];
    State.stateOverride = "searching";
    State.noteActions = null;
    State.noteMessage = FR ? `Lecture de ${host}…` : `Reading ${host}…`;
    this.alert("note");
    this.engine.setState("searching");
    Sound.play("search");
    try {
      const sc = await Bridge.scanUrl(url);
      State.stateOverride = null;
      State.windowContext = null;
      State.textContext = null;
      State.droppedFiles = [{ name: sc.file.name, path: sc.file.path, source: sc.file.source }];
      State.chatHistory = [];
      State.pendingQuery = null;
      State.pendingAction = null;
      void Bridge.chatReset();
      State.prefill = FR
        ? "Explique ce que c'est, à quoi ça sert, et fais-en une analyse."
        : "Explain what this is, what it's for, and analyse it.";
      this.alert("prompt");
      Sound.play("finish");
    } catch (err) {
      State.stateOverride = null;
      State.noteActions = null;
      State.noteMessage = String(err).replace(/^Error:\s*/, "");
      this.alert("note");
      Sound.play("error");
      window.setTimeout(() => { if (State.view === "note") this.setView(State.defaultView()); }, 6000);
    }
  }

  // ── Clipboard & dropped-text offers ─────────────────────────────────────────

  /** Opens the chat on a block of raw text with a chosen action. */
  private runTextAction(action: "correct" | "translate" | "summarize", text: string) {
    State.droppedFiles = [];
    State.windowContext = null;
    State.textContext = text;
    State.chatHistory = [];
    void Bridge.chatReset();
    queueTextAction(action, text);
    State.noteActions = null;
    this.alert("prompt");
  }

  /** Three-way offer (Correct / Translate / Summarize) for a block of text. */
  private offerTextActions(text: string, lead: string) {
    State.stateOverride = null;
    State.noteMessage = lead;
    State.noteActions = [
      { label: TEXT_LABELS.correct, primary: true, run: () => this.runTextAction("correct", text) },
      { label: TEXT_LABELS.translate, run: () => this.runTextAction("translate", text) },
      { label: TEXT_LABELS.summarize, run: () => this.runTextAction("summarize", text) },
      { label: FR ? "Ignorer" : "Dismiss", run: () => { State.noteActions = null; this.collapse(); } },
    ];
    this.alert("note");
    this.fsm.pinned = true;
    Sound.play("peek");
    window.setTimeout(() => {
      if (State.view === "note" && State.noteActions) {
        State.noteActions = null;
        this.fsm.pinned = false;
        this.collapse();
      }
    }, 9000);
  }

  /** The clipboard watcher found freshly-copied text or a link. */
  onClipboard(e: { kind: "url" | "text"; preview: string; chars: number; text: string }) {
    // Never interrupt an open/busy island, a throw, or a pause.
    if (State.paused || State.throwing || State.mode === "expanded" || UploadSeq.isActive) return;
    if (e.kind === "url") {
      State.stateOverride = null;
      State.noteMessage = FR ? `Lien copié : ${e.preview}` : `Link copied: ${e.preview}`;
      State.noteActions = [
        { label: FR ? "Analyser" : "Analyse", primary: true, run: () => void this.scanAndOpen(e.text) },
        { label: FR ? "Corriger" : "Correct", run: () => this.runTextAction("correct", e.text) },
        { label: FR ? "Ignorer" : "Dismiss", run: () => { State.noteActions = null; this.collapse(); } },
      ];
      this.alert("note");
      this.fsm.pinned = true;
      Sound.play("peek");
      window.setTimeout(() => {
        if (State.view === "note" && State.noteActions) {
          State.noteActions = null;
          this.fsm.pinned = false;
          this.collapse();
        }
      }, 9000);
    } else {
      this.offerTextActions(e.text, FR ? `Texte copié (${e.chars}) — je t'aide ?` : `Text copied (${e.chars}) — need a hand?`);
    }
  }

  /** Text (not a file) was dragged onto Mochi. */
  onTextDropped(text: string) {
    State.fileDragOver = false;
    const t = text.trim();
    if (t.length < 2) return;
    this.offerTextActions(t, FR ? "Texte déposé — je t'aide ?" : "Text dropped — need a hand?");
  }

  /** Cursor in window-logical coordinates. */
  onCursor(x: number, y: number) {
    State.mouse = { x, y };
    const rect = this.islandRect();
    State.mouseInIsland = { x: x - rect.x, y: y - rect.y };

    // Windows sends no cursor position with an OLE drag, so the drop sequence is
    // fed from the Win32 cursor poll instead — it runs throughout the drag.
    if (UploadSeq.isActive && !UploadSeq.dropped) {
      UploadSeq.updateCursor(State.mouseInIsland.x, State.mouseInIsland.y);
    }

    const inIsland =
      x >= rect.x - HIT_MARGIN && x <= rect.x + rect.w + HIT_MARGIN &&
      y >= rect.y - HIT_MARGIN && y <= rect.y + rect.h + HIT_MARGIN;

    if (inIsland && !this.wasInIsland) {
      if (this.fsm.state === "coucou") this.greeting.hover();
      this.fsm.mouseEntered();
      this.homeCollapseAt = null;
    }
    if (!inIsland && this.wasInIsland) {
      this.fsm.mouseLeft();
      if (this.fsm.state === "home" && !State.isPinned) {
        this.homeCollapseAt = performance.now() + State.settings.autoCloseInterval * 1000;
      }
    }
    this.wasInIsland = inIsland;

    // Bot hover → love
    const overBot = State.mode === "expanded" && State.stateOverride == null && this.isBotHit(x, y);
    if (overBot && !this.botHovering) this.botHoverIn(x, y);
    if (!overBot && this.botHovering) this.cancelBotHover();
    this.botHovering = overBot;
    if (this.botHovering) {
      const d = Math.hypot(x - this.botHoverStart.x, y - this.botHoverStart.y);
      if (d > 40) {
        this.botHoverStart = { x, y };
        this.scheduleLove();
      }
    }

    this.ensureRunning();
  }

  private isBotHit(x: number, y: number): boolean {
    const rect = this.islandRect();
    const cx = rect.x + this.botCx.value;
    const cy = rect.y + this.botCy.value;
    const radius = this.botSize.value / 2;
    return (x - cx) ** 2 + (y - cy) ** 2 <= radius * radius;
  }

  private botHoverIn(x: number, y: number) {
    if (performance.now() / 1000 - this.lastLoveTime < 6) return;
    this.botHoverStart = { x, y };
    this.engine.blink();
    this.engine.tgEs = 1.08;
    Sound.play("hover");
    this.scheduleLove();
  }

  private scheduleLove() {
    if (this.botHoverTimer != null) window.clearTimeout(this.botHoverTimer);
    this.botHoverTimer = window.setTimeout(() => {
      this.botHoverTimer = null;
      if (!this.botHovering || State.stateOverride != null) return;
      if (performance.now() / 1000 - this.lastLoveTime < 6) return;
      this.lastLoveTime = performance.now() / 1000;
      this.engine.triggerEmote("love");
      Sound.play("love");
    }, 1900);
  }

  private cancelBotHover() {
    if (this.botHoverTimer != null) window.clearTimeout(this.botHoverTimer);
    this.botHoverTimer = null;
    this.engine.tgEs = 1;
  }

  /** Three slaps → dizzy + confused view for 3.3 s, then back. */
  private handleDizzy() {
    this.prevViewBeforeConfused = State.view;
    State.stateOverride = "dizzy";
    this.engine.setState("dizzy");
    Sound.play("dizzy");
    this.alert("confused");
    if (this.confusedRecovery != null) window.clearTimeout(this.confusedRecovery);
    this.confusedRecovery = window.setTimeout(() => {
      this.confusedRecovery = null;
      State.stateOverride = null;
      this.engine.setState(State.effectiveState);
      if (State.view === "confused") {
        const fallback = State.defaultView();
        this.setView(this.prevViewBeforeConfused === "confused" ? fallback : this.prevViewBeforeConfused);
      }
      this.engine.triggerEmote("happy");
    }, 3300);
  }

  // ── Frame loop ──────────────────────────────────────────────────────────────

  ensureRunning() {
    if (this.running) return;
    this.running = true;
    this.lastFrame = performance.now();
    requestAnimationFrame(this.frame);
  }

  private frame = (nowMs: number) => {
    const dt = Math.min(0.05, (nowMs - this.lastFrame) / 1000);
    this.lastFrame = nowMs;

    this.width.step(dt, nowMs);
    this.height.step(dt, nowMs);
    this.radius.step(dt, nowMs);
    this.applyGeometry();

    if (this.dirty) {
      this.dirty = false;
      this.syncDom();
    }

    this.updateBotTargets();
    this.botCx.step(dt);
    this.botCy.step(dt);
    this.botSize.step(dt);

    const greetingActive = State.mode === "expanded" && State.view === "greeting";
    if (greetingActive) {
      const gctx = this.greetingCanvas.getContext("2d");
      if (gctx) {
        const dpr = Math.min(2, window.devicePixelRatio || 1);
        gctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        this.greeting.draw(gctx);
      }
    } else {
      // Kept running even while the drop canvas is up, so the island's own Mochi
      // is already in the right place the moment the canvas fades out.
      this.drawBot(dt);
    }

    const uploadActive = this.uploadActive;
    if (uploadActive) this.uploadCanvas.draw(UploadSeq.frame(), nowMs / 1000);
    this.uploadCanvas.el.classList.toggle("on", uploadActive);
    this.viewsEl.classList.toggle("hidden-by-upload", uploadActive);

    tickMiniBots(dt);
    this.views.get(State.view)?.tick?.(nowMs);
    if (UploadSeq.isActive) this.stepSequence();
    this.updateCountdown(nowMs);

    // Nothing is drawn while the island is hidden, so nothing may keep the loop
    // alive either. This used to read `... || this.engine.busy || State.mode !==
    // "hidden"`, and engine.busy is permanently true for any state with a
    // looping animation — breathing, ratelimit sweat, sleeping z's, the search
    // sweep — so a hidden island went on burning frames in exactly the states it
    // spends most of its life in. Geometry still has to finish retracting.
    const settling =
      this.width.animating || this.height.animating || this.radius.animating;
    const busy = State.mode === "hidden"
      ? settling
      : settling ||
        !this.botCx.settled || !this.botCy.settled || !this.botSize.settled ||
        greetingActive || this.engine.busy || UploadSeq.isActive;

    if (busy) {
      requestAnimationFrame(this.frame);
    } else {
      this.running = false;
      Sound.idle();
    }
  };

  private updateBotTargets() {
    const p = botPosition(State.mode, State.view, this.height.value, State.uploadProgress);
    this.botCx.target = p.cx;
    this.botCy.target = p.cy;
    this.botSize.target = p.diameter / 0.6;

    const greetingActive = State.mode === "expanded" && State.view === "greeting";
    // The drop canvas draws its own Mochi; two of them would overlap.
    const visible = p.opacity > 0 && !greetingActive && !this.uploadActive && !State.throwing;
    this.botCanvas.style.opacity = visible ? "1" : "0";

    if (State.mode === "expanded" && State.view !== "uploading" && !greetingActive && !this.uploadActive) {
      const d = p.diameter;
      const color = botGlowColor(State.effectiveState);
      this.botGlow.style.display = "block";
      this.botGlow.style.width = `${d * 2.2}px`;
      this.botGlow.style.height = `${d * 2.2}px`;
      this.botGlow.style.left = `${this.botCx.value - d * 1.1}px`;
      this.botGlow.style.top = `${this.botCy.value - d * 1.1}px`;
      this.botGlow.style.background = `radial-gradient(circle, ${color} 0%, transparent 62%)`;
      this.botGlow.style.opacity = String(botGlowOpacity(State.effectiveState));
    } else {
      this.botGlow.style.display = "none";
    }
  }

  private drawBot(dt: number) {
    const size = this.botSize.value;
    const w = Math.max(1, Math.round(size));
    const hCss = w + BOT_OVERHANG;
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    if (this.canvasPx !== w) {
      this.canvasPx = w;
      this.botCanvas.width = Math.round(w * dpr);
      this.botCanvas.height = Math.round(hCss * dpr);
      this.botCanvas.style.width = `${w}px`;
      this.botCanvas.style.height = `${hCss}px`;
    }
    this.botCanvas.style.left = `${this.botCx.value - w / 2}px`;
    this.botCanvas.style.top = `${this.botCy.value - BOT_OVERHANG / 2 - hCss / 2}px`;

    const ctx = this.botCanvas.getContext("2d");
    if (!ctx) return;

    const focus = State.focusTask;
    this.engine.bodyColor = focus?.isIntegration ? hexToRGB(focus.color) : null;
    this.engine.particleOverhang = BOT_OVERHANG;
    this.engine.lookX = this.lookX();
    this.engine.lookY = this.lookY();
    if (this.engine.morph > 0.3) {
      this.engine.slotHTarget = State.fileDragOver ? 0.2 : 0;
    } else {
      this.engine.slotHTarget = 0;
      if (this.engine.morph < 0.05) {
        this.engine.slotH = 0;
        this.engine.slotHVel = 0;
      }
    }
    this.engine.update(dt);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, hCss);
    this.engine.draw(ctx, w, hCss);
  }

  /** BotCanvasView.lookX / lookY — tanh of the distance to the bot. */
  private lookX(): number {
    const rect = this.islandRect();
    const botScreenX = rect.x + this.botCx.value;
    return Math.tanh((State.mouse.x - botScreenX) / 260);
  }

  private lookY(): number {
    return -Math.tanh((State.mouse.y - this.botCy.value) / 200);
  }

  private updateCountdown(nowMs: number) {
    if (State.mode !== "expanded" || State.isPinned || this.homeCollapseAt == null) {
      this.countdown.style.width = "0px";
      return;
    }
    const autoClose = State.settings.autoCloseInterval;
    const windowS = Math.min(10, autoClose * 0.6);
    const remaining = (this.homeCollapseAt - nowMs) / 1000;
    this.countdown.style.width =
      remaining < windowS ? `${Math.max(0, clamp(remaining / windowS, 0, 1) * 160)}px` : "0px";
  }

  // ── DOM sync ────────────────────────────────────────────────────────────────

  private syncDom() {
    const expanded = State.mode === "expanded";
    const greetingActive = expanded && State.view === "greeting";

    this.contentEl.style.opacity = expanded && !greetingActive ? "1" : "0";
    this.contentEl.style.pointerEvents = expanded && !greetingActive ? "auto" : "none";
    this.greetingCanvas.style.display = greetingActive ? "block" : "none";

    this.header.sync();
    for (const [name, view] of this.views) {
      const on = name === State.view;
      view.el.classList.toggle("on", on);
      if (on) view.sync();
    }

    // The chat is the only view with a text field, so it is the only time the
    // island is allowed to take keyboard focus.
    if (this.lastSyncedView !== State.view) {
      const wasChat = this.lastSyncedView === "prompt";
      this.lastSyncedView = State.view;
      if (State.view === "prompt") {
        void Bridge.focusWindow(true);
        window.setTimeout(() => this.views.get("prompt")?.focus?.(), 120);
      } else if (wasChat) {
        void Bridge.focusWindow(false);
      }
    }

    // Compact mini grid
    const showGrid = State.mode === "compact";
    this.miniGrid.style.opacity = showGrid ? "1" : "0";
    if (showGrid) {
      const others = State.otherTasks.slice(0, 4);
      const key = others.map((t) => t.id).join("|");
      if (this.miniGrid.dataset.key !== key) {
        this.miniGrid.dataset.key = key;
        this.miniGrid.replaceChildren();
        for (const t of others) {
          this.miniGrid.append(createMiniBot(t, 13));
        }
        pruneMiniBots();
      }
    }

    syncMiniBotStates(State.tasks);
    // Contextual mood: when nothing is going on and Mochi has been sitting idle
    // in the compact peek for a while, let him doze off.
    let mood = State.effectiveState;
    if (State.settings.moodReactions && mood === "idle" && State.mode === "compact") {
      if (performance.now() - State.lastActivity > 25_000) mood = "sleeping";
    }
    this.engine.setState(mood);
  }

  /** Applies settings coming from Rust at boot. */
  applySettings() {
    Sound.setEnabled(State.settings.soundEnabled);
    Sound.setVolume(State.settings.soundVolume);
    this.engine.outfit = (State.settings.outfit ?? "none") as Outfit;
    this.fsm.homeToPetitDelay = State.settings.autoCloseInterval;
    this.applyPhoneLink();
    State.notify();
  }

  get panelSize() {
    return { w: PANEL_W, h: PANEL_H };
  }

  get chatHeight() {
    return chatPromptHeight(State.chatHistory.length);
  }
}
