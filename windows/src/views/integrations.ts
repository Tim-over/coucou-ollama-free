// Integration cards shown in the overview's left card — DOM ports of
// IntegrationCardView and friends from IslandViewContent.swift.
//
// Cal.com is the one simplification: macOS shows a three-level calendar
// (month → day → booking); here it is the list of upcoming bookings.

import { h, svg, clear, dot } from "./dom";
import { ICONS } from "./icons";
import { FR, State, type AgentTask } from "../core/state";
import { Bridge } from "../core/bridge";

/** Same shape as the Swift `timeAgo` computed properties. */
export function timeAgo(value: unknown): string {
  const date = typeof value === "number" ? new Date(value) : new Date(String(value));
  const diff = (Date.now() - date.getTime()) / 1000;
  if (!Number.isFinite(diff)) return "";
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  return `${Math.floor(diff / 86400)}d`;
}

function header(color: string, name: string, kind: string, extra?: Node): HTMLElement {
  const row = h("div", { class: "int-head" }, dot(color, 7), h("b", { text: name }), h("span", { text: kind }));
  if (extra) row.append(extra);
  return row;
}

/** Highlighted first row + plain rows, the layout every list card shares. */
function listRow(accent: string, first: boolean, ...children: Node[]): HTMLElement {
  const row = h("div", { class: first ? "int-row first" : "int-row" }, dot(accent, 5), ...children);
  if (first) row.style.background = `${accent}14`;
  return row;
}

function get(id: string): Record<string, unknown> {
  return (State.integrations[id]?.data ?? {}) as Record<string, unknown>;
}

function arr(id: string, key: string): Record<string, unknown>[] {
  const v = get(id)[key];
  return Array.isArray(v) ? (v as Record<string, unknown>[]) : [];
}

// ── Not configured / idle ─────────────────────────────────────────────────────

const OPEN_URLS: Record<string, string> = {
  integration_resend: "https://resend.com/emails",
  integration_vercel: "https://vercel.com/dashboard",
  integration_github: "https://github.com",
  integration_stripe: "https://dashboard.stripe.com/payments",
  integration_notion: "https://notion.so",
  integration_calcom: "https://app.cal.com/bookings",
};

function idleCard(task: AgentTask, openSettings: () => void): HTMLElement {
  const info = State.integrations[task.id];
  const configured = info?.configured ?? false;
  const error = info?.error ?? null;
  // The Claude Code pill is about hooks, not a key — the macOS wording would be
  // misleading here.
  const missing = task.id === "integration_claude" ? "Hooks not installed" : "Key not configured";
  const label = error ?? (configured ? "Connected · loading…" : missing);
  const statusColor = error || !configured ? "#F4505E" : "#22C55E";

  const actions = h("div", { class: "int-actions" });
  if (task.id === "integration_claude") {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}b3`,
        text: "Open Visual Studio Code",
        onclick: () => void Bridge.openInVSCode(task.sessionCwd ?? null),
      }),
    );
  } else if (task.id === "integration_n8n") {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: "Open n8n",
        onclick: () => void Bridge.openN8n(),
      }),
    );
  } else if (OPEN_URLS[task.id]) {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: `Open ${task.name}`,
        onclick: () => void Bridge.openUrl(OPEN_URLS[task.id]),
      }),
    );
  }
  if (configured) {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: "Refresh",
        onclick: () => void Bridge.refreshIntegration(task.id),
      }),
    );
  } else {
    actions.append(
      h("button", { class: "link-btn", style: "color:#8e939c", text: "Settings…", onclick: openSettings }),
    );
  }

  return h(
    "div",
    { class: "int-card" },
    header(task.color, task.id === "integration_claude" ? "VS Code" : task.name, "Integration"),
    h("div", { class: "int-status" }, dot(statusColor, 5), h("span", { text: label })),
    actions,
  );
}

// ── Vercel ────────────────────────────────────────────────────────────────────

function vercelCard(onDetail: () => void): HTMLElement {
  const deployments = arr("integration_vercel", "deployments");
  const rows = h("div", { class: "int-rows" });
  deployments.slice(0, 3).forEach((d, i) => {
    const accent = d.state === "READY" ? "#22C55E" : "#F4505E";
    const name = h("span", { class: "int-name", text: String(d.projectName ?? "") });
    const ago = h("span", { class: "int-ago", text: timeAgo(d.createdAt) });
    if (i === 0) {
      const more = h(
        "button",
        { class: "int-more", title: "Details", onclick: onDetail },
        svg(ICONS.ellipsis, 8),
      );
      rows.append(listRow(accent, true, name, ago, more));
    } else {
      rows.append(listRow(accent, false, name, ago));
    }
  });
  return h("div", { class: "int-card" }, header("#7C5CFF", "Vercel", "Deployments"), rows);
}

function vercelDetail(onBack: () => void): HTMLElement {
  const d = arr("integration_vercel", "deployments")[0] ?? {};
  const success = d.state === "READY";
  const accent = success ? "#22C55E" : "#F4505E";
  const status = success ? "Ready" : d.state === "CANCELED" ? "Canceled" : "Error";
  const body = h("div", { class: "int-detail-body" });
  if (d.commitMessage) body.append(h("div", { class: "int-commit", text: String(d.commitMessage) }));
  const meta = h("div", { class: "int-meta" });
  if (d.branch) meta.append(h("span", { text: String(d.branch) }));
  meta.append(h("span", { text: `${timeAgo(d.createdAt)} ago` }));
  body.append(meta);
  if (d.url) {
    body.append(
      h("button", {
        class: "int-link",
        text: String(d.url),
        onclick: () => void Bridge.openUrl(`https://${d.url}`),
      }),
    );
  }
  return h(
    "div",
    { class: "int-card detail" },
    h(
      "div",
      { class: "int-detail-head" },
      h("button", { class: "int-back", onclick: onBack }, svg(ICONS.chevronLeft, 10, { stroke: 2.4 })),
      dot(accent, 6),
      h("b", { text: String(d.projectName ?? "Deployment") }),
      h("span", { class: "int-badge", style: `color:${accent};background:${accent}24`, text: status }),
    ),
    body,
  );
}

// ── Resend ────────────────────────────────────────────────────────────────────

function resendCard(): HTMLElement {
  const emails = arr("integration_resend", "emails");
  const total = get("integration_resend").total;
  const extra =
    total != null
      ? h("span", { class: "int-total" }, h("i", { class: "pulse" }), h("span", { text: String(total) }))
      : undefined;
  const rows = h("div", { class: "int-rows" });
  emails.slice(0, 3).forEach((e, i) => {
    const delivered = e.lastEvent === "delivered";
    const accent = delivered ? "#22C55E" : "#F4505E";
    const to = Array.isArray(e.to) ? String(e.to[0] ?? "?") : "?";
    const short = to.split("@")[0];
    const cells: Node[] = [
      h("span", { class: "int-name", text: short }),
      h("span", { class: "int-ago", text: timeAgo(e.createdAt) }),
    ];
    if (i === 0 && e.subject) cells.push(h("span", { class: "int-sub", text: String(e.subject) }));
    rows.append(listRow(accent, i === 0, ...cells));
  });
  return h("div", { class: "int-card" }, header("#22C55E", "Resend", "Emails", extra), rows);
}

// ── GitHub ────────────────────────────────────────────────────────────────────

function statRow(icon: string, color: string, label: string, value: string): HTMLElement {
  return h(
    "div",
    { class: "int-stat" },
    h("i", { class: "int-stat-icon", style: `color:${color}` }, svg(icon, 10)),
    h("span", { class: "int-stat-label", text: label }),
    h("span", { class: "int-stat-value", text: value }),
  );
}


/** Push-to-GitHub panel: pick a folder, type a message, commit + push. */
function githubPushPanel(): HTMLElement {
  const wrap = h("div", { class: "gh-push" });
  const t = (fr: string, en: string) => (FR ? fr : en);

  let folder: string | null = null;
  let busy = false;
  // null until we know; then whether the folder already has a GitHub origin and
  // the repo name to create if it doesn't.
  let hasOrigin: boolean | null = null;
  let repoName = "";

  function reset() {
    folder = null;
    busy = false;
    hasOrigin = null;
    repoName = "";
    draw();
  }

  function draw() {
    clear(wrap);
    if (!folder) {
      wrap.append(
        h("button", {
          class: "link-btn",
          style: "color:#F4505Ed9",
          text: t("Pousser un projet…", "Push a project…"),
          onclick: pick,
        }),
      );
      return;
    }
    const name = folder.split(/[\\/]/).filter(Boolean).pop() || folder;

    if (hasOrigin === null) {
      wrap.append(h("div", { class: "gh-status", text: t(`Vérification de ${name}…`, `Checking ${name}…`) }));
      return;
    }

    const creating = !hasOrigin;
    const msg = h("input", {
      type: "text",
      class: "gh-msg",
      placeholder: t("Message du commit", "Commit message"),
      spellcheck: "false",
    }) as HTMLInputElement;
    const first = creating
      ? t(`Nouveau dépôt « ${repoName} » (public) à créer.`, `New repo “${repoName}” (public) to create.`)
      : t(`Dossier : ${name}`, `Folder: ${name}`);
    const status = h("div", { class: "gh-status", text: first });
    const go = h("button", { class: "link-btn", style: "color:#22C55E", text: creating ? t("Créer et pousser", "Create & push") : t("Pousser", "Push") });
    const cancel = h("button", { class: "link-btn", style: "color:#8e939c", text: t("Annuler", "Cancel") });

    const run = async () => {
      if (busy || !folder) return;
      busy = true;
      go.disabled = true;
      cancel.disabled = true;
      status.textContent = creating ? t("Création sur GitHub…", "Creating on GitHub…") : t("Envoi vers GitHub…", "Pushing to GitHub…");
      status.className = "gh-status";
      try {
        const r = creating
          ? await Bridge.gitCreateRepo(folder, msg.value, true)
          : await Bridge.gitPush(folder, msg.value);
        status.textContent = r.summary;
        status.className = "gh-status ok";
        go.textContent = t("Fermer", "Close");
        go.disabled = false;
        go.onclick = reset;
        cancel.style.display = "none";
      } catch (err) {
        status.textContent = String(err).replace(/^Error:\s*/, "");
        status.className = "gh-status err";
        go.disabled = false;
        cancel.disabled = false;
      } finally {
        busy = false;
      }
    };
    go.addEventListener("click", () => void run());
    cancel.addEventListener("click", reset);
    msg.addEventListener("keydown", (e) => {
      if ((e as KeyboardEvent).key === "Enter") { e.preventDefault(); void run(); }
      e.stopPropagation();
    });

    wrap.append(status, h("div", { class: "gh-row" }, msg), h("div", { class: "int-actions" }, go, cancel));
    requestAnimationFrame(() => msg.focus());
  }

  async function pick() {
    try {
      const picked = await Bridge.pickFolder();
      if (!picked) return;
      folder = picked;
      hasOrigin = null;
      draw();
      try {
        const st = await Bridge.gitRepoState(picked);
        hasOrigin = st.hasOrigin;
        repoName = st.suggestedName;
      } catch {
        hasOrigin = false; // treat an unknown state as "offer to create"
      }
      draw();
    } catch {
      /* dialog cancelled or unavailable */
    }
  }

  draw();
  return wrap;
}

function ciDot(ci: string): HTMLElement {
  const color = ci === "success" ? "#22C55E" : ci === "failure" ? "#F4505E" : ci === "pending" ? "#F5A524" : "#6B7079";
  return h("i", { style: `display:inline-block;width:8px;height:8px;border-radius:50%;flex:0 0 auto;background:${color}`, title: ci });
}

function prRow(pr: Record<string, unknown>, showCi: boolean): HTMLElement {
  const url = String(pr.url ?? "");
  const row = h("div", { class: "int-row gh-pr", title: `${pr.repo} #${pr.number}` });
  if (showCi) row.append(ciDot(String(pr.ci ?? "none")));
  row.append(
    h("div", { style: "min-width:0;flex:1 1 auto" },
      h("div", { class: "int-row-title", text: String(pr.title ?? ""), style: "white-space:nowrap;overflow:hidden;text-overflow:ellipsis" }),
      h("div", { class: "int-sub", text: `${pr.repo} #${pr.number}` }),
    ),
  );
  if (url) row.addEventListener("click", () => void Bridge.openUrl(url));
  return row;
}

/** Last-7-days contribution sparkline + total. */
function contribStrip(days: number[], total: number): HTMLElement {
  const max = Math.max(1, ...days);
  const cells = h("div", { class: "gh-contrib" });
  for (const d of days) {
    const t = d / max;
    const bg = d === 0 ? "rgba(255,255,255,0.07)"
      : `rgba(34,197,94,${(0.35 + t * 0.65).toFixed(2)})`;
    cells.append(h("i", { class: "gh-cell", style: `background:${bg}`, title: `${d}` }));
  }
  return h("div", { class: "gh-contrib-row" },
    cells,
    h("span", { class: "int-sub", text: FR ? `${total} contributions / an` : `${total} contributions / yr` }),
  );
}

function githubCard(): HTMLElement {
  const d = get("integration_github");
  const repos = Number(d.totalRepos ?? 0);
  const prs = (Array.isArray(d.prs) ? d.prs : []) as Record<string, unknown>[];
  const reviews = (Array.isArray(d.reviews) ? d.reviews : []) as Record<string, unknown>[];
  const days = (Array.isArray(d.contribDays) ? d.contribDays : []) as number[];
  const total = Number(d.contribTotal ?? 0);

  const body = h("div", { style: "display:flex;flex-direction:column;gap:10px" });

  if (days.length) body.append(contribStrip(days, total));

  body.append(
    h("div", { class: "int-stats" },
      statRow(ICONS.stack, "#6B7079", "Repositories", String(repos)),
      statRow(ICONS.bang, "#F5A524", FR ? "À relire" : "To review", String(reviews.length)),
    ),
  );

  if (prs.length) {
    body.append(h("div", { class: "int-sub", style: "margin-top:2px", text: FR ? "Mes pull requests" : "My pull requests" }));
    const list = h("div", { class: "int-rows tight" });
    for (const pr of prs) list.append(prRow(pr, true));
    body.append(list);
  }
  if (reviews.length) {
    body.append(h("div", { class: "int-sub", style: "margin-top:2px", text: FR ? "À relire" : "Awaiting your review" }));
    const list = h("div", { class: "int-rows tight" });
    for (const pr of reviews) list.append(prRow(pr, false));
    body.append(list);
  }
  if (!prs.length && !reviews.length) {
    body.append(h("div", { class: "hint", text: FR ? "Aucune pull request ouverte." : "No open pull requests." }));
  }

  return h("div", { class: "int-card" },
    header("#F4505E", "GitHub", "Overview"),
    body,
    githubPushPanel(),
  );
}

// ── Stripe// ── Stripe ────────────────────────────────────────────────────────────────────

function stripeCard(): HTMLElement {
  const d = get("integration_stripe");
  const balance = (Number(d.balance ?? 0) / 100).toFixed(2);
  const currency = String(d.currency ?? "eur").toUpperCase();
  const rows = h("div", { class: "int-rows tight" });
  for (const p of arr("integration_stripe", "payments")) {
    const success = p.status === "succeeded";
    const accent = success ? "#22C55E" : "#F4505E";
    rows.append(
      h(
        "div",
        { class: "int-row" },
        dot(accent, 5),
        h("span", { class: "int-name", text: String(p.description ?? "Payment") }),
        h("span", {
          class: "int-amount",
          style: "color:#22c55e",
          text: `+${(Number(p.amount ?? 0) / 100).toFixed(2)}`,
        }),
        h("span", { class: "int-ago", text: timeAgo(p.createdAt) }),
      ),
    );
  }
  return h(
    "div",
    { class: "int-card" },
    header("#0570DE", "Stripe", "Payments"),
    h("div", { class: "int-balance" }, h("span", { text: balance }), h("i", { text: currency })),
    rows,
  );
}

// ── Notion ────────────────────────────────────────────────────────────────────

function notionCard(): HTMLElement {
  const rows = h("div", { class: "int-rows tight" });
  for (const p of arr("integration_notion", "pages").slice(0, 3)) {
    rows.append(
      h(
        "button",
        {
          class: "int-page",
          onclick: () => {
            if (typeof p.url === "string") void Bridge.openUrl(p.url);
          },
        },
        p.emoji
          ? h("span", { class: "int-emoji", text: String(p.emoji) })
          : h("i", { class: "int-emoji" }, svg(ICONS.doc, 9)),
        h("span", { class: "int-name", text: String(p.title ?? "Untitled") }),
        h("span", { class: "int-ago", text: timeAgo(p.lastEditedAt) }),
      ),
    );
  }
  return h("div", { class: "int-card" }, header("#E8E8E8", "Notion", "Recent"), rows);
}

// ── Cal.com ───────────────────────────────────────────────────────────────────

function calcomCard(): HTMLElement {
  const bookings = arr("integration_calcom", "bookings")
    .slice()
    .sort((a, b) => new Date(String(a.start)).getTime() - new Date(String(b.start)).getTime());
  const rows = h("div", { class: "int-rows tight" });
  if (bookings.length === 0) {
    rows.append(h("div", { class: "int-empty", text: "No calls scheduled" }));
  }
  for (const b of bookings.slice(0, 3)) {
    const when = new Date(String(b.start));
    const day = when.toLocaleDateString(undefined, { day: "2-digit", month: "2-digit" });
    const time = when.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
    rows.append(
      h(
        "div",
        { class: "int-row" },
        dot("#C9956A", 4),
        h("span", { class: "int-time", text: `${day} ${time}` }),
        h("span", { class: "int-name", text: String(b.title ?? "Meeting") }),
      ),
    );
  }
  return h("div", { class: "int-card" }, header("#C9956A", "Cal.com", "Schedule"), rows);
}

// ── n8n ───────────────────────────────────────────────────────────────────────

function n8nCard(task: AgentTask, onDetail: () => void, openSettings: () => void): HTMLElement {
  const hasActivity = task.steps.length > 0 && (task.state === "finished" || task.state === "error");
  if (!hasActivity) return idleCard(task, openSettings);
  const success = task.state === "finished";
  const accent = success ? "#22C55E" : "#F4505E";
  return h(
    "div",
    { class: "int-card" },
    header("#F29B38", "n8n", "Workflow"),
    h(
      "div",
      { class: "int-actions" },
      h(
        "button",
        {
          class: "int-pill",
          style: `background:${accent}1a;border-color:${accent}38`,
          onclick: onDetail,
        },
        dot(accent, 5),
        h("span", { class: "int-name", text: task.steps[0] ?? "Workflow" }),
        svg(ICONS.ellipsis, 8),
      ),
    ),
  );
}

function n8nDetail(task: AgentTask, onBack: () => void): HTMLElement {
  const success = task.state === "finished";
  const accent = success ? "#22C55E" : "#F4505E";
  const detail = task.steps[1];
  return h(
    "div",
    { class: "int-card detail" },
    h(
      "div",
      { class: "int-detail-head" },
      h("button", { class: "int-back", onclick: onBack }, svg(ICONS.chevronLeft, 10, { stroke: 2.4 })),
      dot(accent, 6),
      h("b", { text: task.steps[0] ?? "Workflow" }),
      h("span", {
        class: "int-badge",
        style: `color:${accent};background:${accent}24`,
        text: success ? "Success" : "Failed",
      }),
    ),
    detail
      ? h("pre", { class: "int-detail-text", text: detail })
      : h("div", {
          class: "int-status",
          text: success ? "Completed successfully." : "No error details available.",
        }),
  );
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

export interface IntegrationCardHooks {
  detailOpen: boolean;
  openDetail(): void;
  closeDetail(): void;
  openSettings(): void;
}

/** True when this integration has data worth showing instead of the idle card. */
export function hasIntegrationData(id: string): boolean {
  const info = State.integrations[id];
  if (!info || info.error) return false;
  switch (id) {
    case "integration_vercel":
      return arr(id, "deployments").length > 0;
    case "integration_resend":
      return arr(id, "emails").length > 0;
    case "integration_github":
      return get(id).totalRepos != null;
    case "integration_stripe":
      return info.loaded;
    case "integration_notion":
      return arr(id, "pages").length > 0;
    case "integration_calcom":
      return info.loaded;
    default:
      return false;
  }
}

export function renderIntegrationCard(task: AgentTask, hooks: IntegrationCardHooks): HTMLElement {
  if (task.id === "integration_n8n") {
    const hasActivity = task.steps.length > 0 && (task.state === "finished" || task.state === "error");
    return hooks.detailOpen && hasActivity
      ? n8nDetail(task, hooks.closeDetail)
      : n8nCard(task, hooks.openDetail, hooks.openSettings);
  }
  if (task.id === "integration_vercel" && hasIntegrationData(task.id)) {
    return hooks.detailOpen ? vercelDetail(hooks.closeDetail) : vercelCard(hooks.openDetail);
  }
  if (!hasIntegrationData(task.id)) return idleCard(task, hooks.openSettings);

  switch (task.id) {
    case "integration_resend":
      return resendCard();
    case "integration_github":
      return githubCard();
    case "integration_stripe":
      return stripeCard();
    case "integration_notion":
      return notionCard();
    case "integration_calcom":
      return calcomCard();
    default:
      return idleCard(task, hooks.openSettings);
  }
}

export { clear };
