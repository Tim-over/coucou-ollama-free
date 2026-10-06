// Phone link — mirrors agent sessions and approval requests to Firebase so the
// Android companion app can show them and approve/deny from anywhere.
//
// This is the cross-platform twin of the iPhone's CloudKit link: CloudKit is
// Apple-only, so Android pairs with Coucou through a Firebase project the user
// owns. Config (project id + web API key) and a random pair id live in Settings;
// the pair id is the shared secret that scopes everything under pairs/{pairId}.
//
// Transport is the Firestore REST API (no SDK, keeps the bundle small):
//   • anonymous sign-in                    → an idToken we refresh as needed
//   • PATCH pairs/{id}/mirror/state        → the current sessions + approval
//   • GET   pairs/{id}/decisions           → allow/deny the phone wrote, applied
//                                            then deleted so they never replay
// The phone writes decisions and registers its push token; a Cloud Function
// turns a new approval into an FCM push (see firebase/ in the repo).

const AUTH = "https://identitytoolkit.googleapis.com/v1";
const TOKEN = "https://securetoken.googleapis.com/v1";

export interface PhoneConfig {
  apiKey: string;
  projectId: string;
  pairId: string;
}

export interface SessionMirror {
  pillId: string;
  name: string;
  color: string;
  state: string;
  step: string;
  needsApproval: boolean;
  approvalId: string;
  approvalCommand: string;
}

export interface Snapshot {
  pcName: string;
  sessions: SessionMirror[];
  updatedAt: number;
}

export interface Decision {
  docName: string;
  requestId: string;
  decision: "allow" | "deny";
}

// ── Firestore value (de)serialisation ─────────────────────────────────────────

type FsValue = Record<string, unknown>;

function toValue(v: unknown): FsValue {
  if (typeof v === "string") return { stringValue: v };
  if (typeof v === "boolean") return { booleanValue: v };
  if (typeof v === "number") return Number.isInteger(v) ? { integerValue: String(v) } : { doubleValue: v };
  if (Array.isArray(v)) return { arrayValue: { values: v.map(toValue) } };
  if (v && typeof v === "object") return { mapValue: { fields: toFields(v as Record<string, unknown>) } };
  return { nullValue: null };
}

function toFields(obj: Record<string, unknown>): Record<string, FsValue> {
  const out: Record<string, FsValue> = {};
  for (const [k, val] of Object.entries(obj)) out[k] = toValue(val);
  return out;
}

function fromValue(v: FsValue): unknown {
  if (v == null) return null;
  if ("stringValue" in v) return v.stringValue as string;
  if ("booleanValue" in v) return v.booleanValue as boolean;
  if ("integerValue" in v) return Number(v.integerValue);
  if ("doubleValue" in v) return v.doubleValue as number;
  if ("nullValue" in v) return null;
  if ("arrayValue" in v) return ((v.arrayValue as { values?: FsValue[] }).values ?? []).map(fromValue);
  if ("mapValue" in v) return fromFields((v.mapValue as { fields?: Record<string, FsValue> }).fields ?? {});
  return null;
}

function fromFields(fields: Record<string, FsValue>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, val] of Object.entries(fields)) out[k] = fromValue(val);
  return out;
}

// ── The link ──────────────────────────────────────────────────────────────────

export class PhoneLink {
  private cfg: PhoneConfig | null = null;
  private idToken = "";
  private refreshToken = "";
  private tokenExpiry = 0;
  private timer = 0;
  private lastPublished = "";
  private running = false;
  /** Approval ids currently mirrored, so we create/delete the push-trigger docs once. */
  private publishedApprovals = new Set<string>();

  /** Called when the phone approves/denies — the island applies it. */
  onDecision: (d: Decision) => void = () => {};
  /** Surfaced to the settings UI (status line / errors). */
  onStatus: (text: string) => void = () => {};

  get active(): boolean {
    return this.running;
  }

  async start(cfg: PhoneConfig) {
    this.stop();
    if (!cfg.apiKey || !cfg.projectId || !cfg.pairId) {
      this.onStatus("Not configured");
      return;
    }
    this.cfg = cfg;
    this.running = true;
    try {
      await this.signInAnonymously();
      this.onStatus("Linked");
      // Poll for decisions the phone writes (approvals need a quick round-trip).
      this.timer = window.setInterval(() => void this.poll(), 2500) as unknown as number;
    } catch (err) {
      this.running = false;
      this.onStatus(`Link failed: ${String(err)}`);
    }
  }

  stop() {
    this.running = false;
    if (this.timer) window.clearInterval(this.timer);
    this.timer = 0;
    this.cfg = null;
    this.idToken = "";
  }

  /** Mirrors the current snapshot to Firestore (skips a no-op write). */
  async publish(snap: Snapshot) {
    if (!this.running || !this.cfg) return;
    const json = JSON.stringify(snap);
    if (json === this.lastPublished) return;
    this.lastPublished = json;
    try {
      await this.ensureToken();
      const url = `${this.docBase()}/mirror/state?updateMask.fieldPaths=pcName&updateMask.fieldPaths=sessions&updateMask.fieldPaths=updatedAt`;
      await this.fetchJson(url, {
        method: "PATCH",
        body: JSON.stringify({ fields: toFields(snap as unknown as Record<string, unknown>) }),
      });
      await this.syncApprovalDocs(snap);
    } catch (err) {
      this.onStatus(`Publish failed: ${String(err)}`);
    }
  }

  /**
   * One doc per pending approval under approvals/{id}. Creating it fires the
   * Cloud Function that sends the phone a push; deleting it (once answered)
   * lets the function clear the phone's notification.
   */
  private async syncApprovalDocs(snap: Snapshot) {
    const current = new Set(snap.sessions.filter((s) => s.needsApproval && s.approvalId).map((s) => s.approvalId));
    for (const s of snap.sessions) {
      if (!s.needsApproval || !s.approvalId || this.publishedApprovals.has(s.approvalId)) continue;
      const fields = toFields({ requestId: s.approvalId, title: s.name, command: s.approvalCommand, createdAt: Date.now() });
      await this.fetchJson(`${this.docBase()}/approvals?documentId=${encodeURIComponent(s.approvalId)}`, {
        method: "POST",
        body: JSON.stringify({ fields }),
      }).catch(() => {});
      this.publishedApprovals.add(s.approvalId);
    }
    for (const id of [...this.publishedApprovals]) {
      if (current.has(id)) continue;
      await this.fetchJson(`${this.docBase()}/approvals/${encodeURIComponent(id)}`, { method: "DELETE" }).catch(() => {});
      this.publishedApprovals.delete(id);
    }
  }

  private async poll() {
    if (!this.running || !this.cfg) return;
    try {
      await this.ensureToken();
      const list = (await this.fetchJson(`${this.docBase()}/decisions`, { method: "GET" })) as {
        documents?: { name: string; fields: Record<string, FsValue> }[];
      };
      for (const doc of list.documents ?? []) {
        const f = fromFields(doc.fields) as { requestId?: string; decision?: string };
        if (f.requestId && (f.decision === "allow" || f.decision === "deny")) {
          this.onDecision({ docName: doc.name, requestId: f.requestId, decision: f.decision });
        }
        // Consume it so it never replays.
        await this.fetchJson(`https://firestore.googleapis.com/v1/${doc.name}`, { method: "DELETE" }).catch(() => {});
      }
    } catch {
      /* transient: the next tick retries */
    }
  }

  // ── Auth ──────────────────────────────────────────────────────────────────

  private async signInAnonymously() {
    const r = (await this.rawJson(`${AUTH}/accounts:signUp?key=${this.cfg!.apiKey}`, {
      method: "POST",
      body: JSON.stringify({ returnSecureToken: true }),
    })) as { idToken: string; refreshToken: string; expiresIn: string };
    this.idToken = r.idToken;
    this.refreshToken = r.refreshToken;
    this.tokenExpiry = Date.now() + Number(r.expiresIn) * 1000 - 60_000;
  }

  private async ensureToken() {
    if (this.idToken && Date.now() < this.tokenExpiry) return;
    if (!this.refreshToken) return this.signInAnonymously();
    const r = (await this.rawJson(`${TOKEN}/token?key=${this.cfg!.apiKey}`, {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: `grant_type=refresh_token&refresh_token=${encodeURIComponent(this.refreshToken)}`,
    })) as { id_token: string; refresh_token: string; expires_in: string };
    this.idToken = r.id_token;
    this.refreshToken = r.refresh_token;
    this.tokenExpiry = Date.now() + Number(r.expires_in) * 1000 - 60_000;
  }

  // ── HTTP ────────────────────────────────────────────────────────────────────

  private docBase(): string {
    const c = this.cfg!;
    return `https://firestore.googleapis.com/v1/projects/${c.projectId}/databases/(default)/documents/pairs/${c.pairId}`;
  }

  private async fetchJson(url: string, init: RequestInit): Promise<unknown> {
    const res = await fetch(url, {
      ...init,
      headers: { "content-type": "application/json", authorization: `Bearer ${this.idToken}`, ...(init.headers ?? {}) },
    });
    if (!res.ok) throw new Error(`${res.status} ${await res.text()}`);
    return res.status === 204 ? {} : res.json();
  }

  private async rawJson(url: string, init: RequestInit): Promise<unknown> {
    const res = await fetch(url, { ...init, headers: { "content-type": "application/json", ...(init.headers ?? {}) } });
    if (!res.ok) throw new Error(`${res.status} ${await res.text()}`);
    return res.json();
  }
}

/** A fresh random pair id (the shared secret). */
export function newPairId(): string {
  const bytes = new Uint8Array(18);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}
