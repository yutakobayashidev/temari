import "./styles.css";
import {
  applyManagedWorkspace,
  chooseConfig,
  chooseSource,
  chooseTemariExecutable,
  defaultConfigLocation,
  disableManagedSchedule,
  enableManagedSchedule,
  getManagedHistory,
  getManagedSchedule,
  getManagedWorkspace,
  listManagedWorkspaces,
  previewManagedWorkspace,
  proposeManagedWorkspace,
  reprocessManagedFiles,
  runManagedWorkspace,
  setManagedWorkspaceEnabled,
  undoManagedMove,
  undoManagedRun,
} from "./api";
import type {
  ManagedMove,
  ManagedWorkspace,
  ManagedWorkspaceStatus,
  ReprocessArea,
  ScheduleStatus,
  SetupPreview,
  SetupProposal,
} from "./types";

type SetupStep = "source" | "structure" | "preview";
type Notice = { tone: "success" | "error"; message: string };
type PendingConfirmation = {
  title: string;
  copy: string;
  details: Array<[string, string]>;
  confirmLabel: string;
  action: () => Promise<void>;
};

type AppState = {
  workspaces: ManagedWorkspace[];
  selectedId: string | null;
  status: ManagedWorkspaceStatus | null;
  schedule: ScheduleStatus | null;
  history: ManagedMove[];
  configPath: string;
  defaultConfigPath: string;
  scheduleExecutablePath: string;
  busy: boolean;
  notice: Notice | null;
  setupOpen: boolean;
  setupStep: SetupStep;
  setupSource: string;
  proposal: SetupProposal | null;
  setupPreview: SetupPreview | null;
  reprocessOpen: boolean;
  pendingConfirmation: PendingConfirmation | null;
};

const state: AppState = {
  workspaces: [],
  selectedId: null,
  status: null,
  schedule: null,
  history: [],
  configPath: "",
  defaultConfigPath: "",
  scheduleExecutablePath: "",
  busy: true,
  notice: null,
  setupOpen: false,
  setupStep: "source",
  setupSource: "",
  proposal: null,
  setupPreview: null,
  reprocessOpen: false,
  pendingConfirmation: null,
};

const appElement = document.querySelector<HTMLDivElement>("#app");
if (!appElement) throw new Error("App root not found");
const app: HTMLDivElement = appElement;

function escapeHtml(value: string): string {
  const node = document.createElement("span");
  node.textContent = value;
  return node.innerHTML;
}

function escapeAttribute(value: string): string {
  return escapeHtml(value).replaceAll('"', "&quot;");
}

function basename(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;
}

function formatTime(timestamp: number | null): string {
  if (!timestamp) return "Not yet";
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(timestamp);
}

function formatDuration(seconds: number): string {
  if (seconds % 86_400 === 0) return `${seconds / 86_400} days`;
  if (seconds % 3_600 === 0) return `${seconds / 3_600} hours`;
  if (seconds % 60 === 0) return `${seconds / 60} minutes`;
  return `${seconds} seconds`;
}

function formatError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

async function loadSchedule(workspaceId: string): Promise<ScheduleStatus | null> {
  try {
    return await getManagedSchedule(workspaceId);
  } catch {
    return null;
  }
}

function healthLabel(status: ManagedWorkspaceStatus): string {
  if (status.health === "attention") return "Needs attention";
  if (status.health === "disabled") return "Paused";
  return "Watching";
}

function historyRows(): string {
  if (state.history.length === 0) {
    return `<div class="empty-state"><strong>No moves yet</strong><span>Run this workspace to start a reversible move history.</span></div>`;
  }
  return state.history.map((move) => `
    <article class="move-row ${move.undone ? "is-undone" : ""}">
      <div class="move-time">${escapeHtml(formatTime(move.finishedUnixMs))}</div>
      <div class="move-paths">
        <strong>${escapeHtml(move.sourcePath)}</strong>
        <span aria-hidden="true">→</span>
        <strong>${escapeHtml(move.destinationPath)}</strong>
      </div>
      <span class="move-kind">${move.kind === "classify" ? "Classified" : move.kind === "adopt" ? "Kept" : "Staged"}</span>
      ${move.undone
        ? `<span class="undo-state">Undone</span>`
        : move.kind === "adopt"
          ? `<span class="undo-state">Undo by run</span>`
          : `<button class="quiet-button" data-undo-file="${escapeAttribute(move.moveId)}" data-run-id="${escapeAttribute(move.sessionId)}" type="button">Undo</button>`}
    </article>`).join("");
}

function workspaceNavigation(): string {
  if (state.workspaces.length === 0) {
    return `<div class="workspace-empty">No folders are managed yet.</div>`;
  }
  return state.workspaces.map((workspace) => `
    <button class="workspace-link ${workspace.id === state.selectedId ? "is-selected" : ""}" data-workspace-id="${escapeAttribute(workspace.id)}" type="button">
      <span class="folder-tab" aria-hidden="true"></span>
      <span><strong>${escapeHtml(basename(workspace.source))}</strong><small>${workspace.enabled ? "Watching" : "Paused"}</small></span>
      <i class="health-pin ${workspace.enabled ? "" : "is-paused"}" aria-hidden="true"></i>
    </button>`).join("");
}

function dashboard(): string {
  if (!state.status) {
    return `<main class="empty-dashboard">
      <div class="empty-orbit" aria-hidden="true"><i></i><i></i><i></i></div>
      <p class="eyebrow">Private by default</p>
      <h1>Give loose files<br>a quiet place to land.</h1>
      <p>Each folder gets a protected area, a short waiting room, and an organized library.</p>
      ${state.notice ? `<div class="notice is-${state.notice.tone}" role="status">${escapeHtml(state.notice.message)}</div>` : ""}
      <button class="primary-button" data-open-setup type="button">Add your first folder</button>
    </main>`;
  }

  const { workspace, inbox } = state.status;
  const classified = state.history.filter((move) => move.kind === "classify" && !move.undone).length;
  const keptNote = "Folders and files you choose to leave alone";
  const inboxNote = inbox.nextEligibleUnixMs
    ? `Next review ${formatTime(inbox.nextEligibleUnixMs)}`
    : "Nothing is waiting for review";
  const scheduleOn = state.schedule?.installed && state.schedule.enabled;
  const latestRuns = [...new Set(state.history.filter((move) => !move.undone).map((move) => move.sessionId))].slice(0, 3);

  return `<main class="dashboard">
    <header class="workspace-header">
      <div>
        <div class="health-line"><span class="health-badge is-${state.status.health}">${healthLabel(state.status)}</span><span>${escapeHtml(workspace.source)}</span></div>
        <h1>${escapeHtml(basename(workspace.source))}</h1>
        <p>Files wait ${escapeHtml(formatDuration(workspace.retentionSeconds))} before organization.</p>
      </div>
      <button class="run-button" id="run-now" type="button" ${state.busy || !workspace.enabled ? "disabled" : ""}>
        <span class="run-mark" aria-hidden="true">↻</span>
        <span><strong>${state.busy ? "Working…" : "Run now"}</strong><small>Review, then move</small></span>
      </button>
    </header>

    ${state.notice ? `<div class="notice is-${state.notice.tone}" role="status">${escapeHtml(state.notice.message)}</div>` : ""}
    ${state.status.issues.length ? `<div class="issue-list"><strong>Needs attention</strong>${state.status.issues.map((issue) => `<span>${escapeHtml(issue)}</span>`).join("")}</div>` : ""}

    <section class="areas" aria-labelledby="areas-title">
      <div class="section-heading"><div><p class="eyebrow">Workspace flow</p><h2 id="areas-title">Three places, one clear boundary</h2></div><span>Root → Inbox → Library</span></div>
      <div class="area-flow">
        <article class="area-card area-kept">
          <div class="area-index">K</div>
          <div><p>Leave alone</p><h3>Kept</h3><span>${keptNote}</span></div>
          <strong class="area-value">Protected</strong>
        </article>
        <span class="flow-thread" aria-hidden="true"></span>
        <article class="area-card area-inbox">
          <div class="area-index">I</div>
          <div><p>Wait here</p><h3>Inbox</h3><span>${escapeHtml(inboxNote)}</span></div>
          <strong class="area-value">${inbox.physicalFiles}</strong>
          <small class="area-detail">${inbox.eligibleNow} ready now</small>
        </article>
        <span class="flow-thread" aria-hidden="true"></span>
        <article class="area-card area-library">
          <div class="area-index">L</div>
          <div><p>Organized by meaning</p><h3>Library</h3><span>Approved destinations only</span></div>
          <strong class="area-value">${classified || inbox.indexedMoved}</strong>
          <small class="area-detail">recently indexed</small>
        </article>
      </div>
    </section>

    <div class="dashboard-grid">
      <section class="history-panel" aria-labelledby="history-title">
        <div class="section-heading compact"><div><p class="eyebrow">Recent moves</p><h2 id="history-title">Every move has a way back</h2></div></div>
        <div class="move-list">${historyRows()}</div>
        ${latestRuns.length ? `<div class="session-undo"><span>Undo a complete run</span>${latestRuns.map((runId) => `<button class="text-button" data-undo-run="${escapeAttribute(runId)}" type="button">${escapeHtml(runId)}</button>`).join("")}</div>` : ""}
      </section>

      <aside class="control-panel">
        <section>
          <div class="control-heading"><div><p class="eyebrow">Timing</p><h2>Waiting room</h2></div></div>
          <label class="field"><span>Keep new files in Inbox</span><select id="retention-days" disabled>
            ${[1, 2, 3, 5, 7, 14].map((days) => `<option value="${days}" ${workspace.retentionSeconds === days * 86_400 ? "selected" : ""}>${days} ${days === 1 ? "day" : "days"}</option>`).join("")}
          </select></label>
          <label class="field"><span>Wait until unchanged</span><select id="settle-seconds" disabled>
            ${[10, 30, 60, 300].map((seconds) => `<option value="${seconds}" ${workspace.settleSeconds === seconds ? "selected" : ""}>${formatDuration(seconds)}</option>`).join("")}
          </select></label>
          <label class="toggle-row"><span><strong>Watch this folder</strong><small>Allow scheduled and manual runs</small></span><input id="workspace-enabled" type="checkbox" ${workspace.enabled ? "checked" : ""} /></label>
          <p class="field-note">Timing is shown here; edit it with the managed CLI for now.</p>
          <button class="secondary-button" id="save-settings" type="button">Save watch state</button>
        </section>

        <section>
          <div class="control-heading"><div><p class="eyebrow">Schedule</p><h2>${scheduleOn ? "Runs automatically" : "Runs manually"}</h2></div><span class="schedule-light ${scheduleOn ? "is-on" : ""}"></span></div>
          <label class="field"><span>Check every</span><select id="schedule-interval">
            ${[[300, "5 minutes"], [900, "15 minutes"], [1800, "30 minutes"], [3600, "1 hour"]].map(([seconds, label]) => `<option value="${seconds}" ${state.schedule?.intervalSeconds === seconds ? "selected" : ""}>${label}</option>`).join("")}
          </select></label>
          ${scheduleOn ? "" : `<label class="picker-field compact"><span>Temari CLI executable</span><div><input id="schedule-executable" readonly value="${escapeAttribute(state.scheduleExecutablePath)}" placeholder="Choose a stable CLI path" /><button id="pick-schedule-executable" type="button">Choose</button></div></label><p class="field-note">Use an absolute launcher path outside the Nix store.</p>`}
          <button class="secondary-button" id="toggle-schedule" type="button" ${!scheduleOn && !state.scheduleExecutablePath ? "disabled" : ""}>${scheduleOn ? "Turn off schedule" : "Turn on schedule"}</button>
        </section>

        <section>
          <div class="control-heading"><div><p class="eyebrow">Send back through Inbox</p><h2>Reprocess files</h2></div></div>
          <p class="control-copy">Select files from Kept or Library. Temari creates a reviewed move back to Inbox first.</p>
          <button class="secondary-button" id="open-reprocess" type="button">Choose files to reprocess</button>
        </section>
      </aside>
    </div>
  </main>`;
}

function setupDialog(): string {
  if (!state.setupOpen) return "";
  const sourceStep = state.setupStep === "source";
  const structureStep = state.setupStep === "structure" && state.proposal;
  const previewStep = state.setupStep === "preview" && state.setupPreview;
  return `<dialog class="sheet-dialog" id="setup-dialog" open aria-labelledby="setup-title">
    <div class="sheet-backdrop" data-close-setup></div>
    <section class="sheet-card">
      <button class="dialog-close" data-close-setup aria-label="Close" type="button">×</button>
      <p class="eyebrow">Add a managed folder · ${sourceStep ? "1" : structureStep ? "2" : "3"} of 3</p>
      <h2 id="setup-title">${sourceStep ? "Choose one folder" : structureStep ? "Approve its Library" : "Review the exact setup"}</h2>
      ${sourceStep ? `
        <p>Each folder stays independent and gets its own Kept, Inbox, and Library.</p>
        <label class="picker-field"><span>Folder</span><div><input id="setup-source" readonly value="${escapeAttribute(state.setupSource)}" placeholder="No folder selected" /><button id="pick-setup-source" type="button">Choose</button></div></label>
        <label class="picker-field"><span>Model configuration</span><div><input id="setup-config" readonly value="${escapeAttribute(state.configPath)}" placeholder="No configuration selected" /><button id="pick-setup-config" type="button">Choose</button></div></label>
        <button class="primary-button full" id="propose-workspace" type="button" ${!state.setupSource || !state.configPath || state.busy ? "disabled" : ""}>${state.busy ? "Reading file names…" : "Propose a Library"}</button>` : ""}
      ${structureStep ? `
        <p>${state.proposal!.filesConsidered} file names informed this proposal. Edit every destination before approval.</p>
        <div class="folder-proposal" id="setup-folders">${state.proposal!.folders.map((folder, index) => `
          <fieldset data-folder-index="${index}"><legend>Destination ${index + 1}</legend><input name="path" value="${escapeAttribute(folder.path)}" aria-label="Destination path" /><input name="description" value="${escapeAttribute(folder.description)}" aria-label="Destination purpose" /></fieldset>`).join("")}</div>
        <div class="setup-timing"><label>Inbox retention<select id="setup-retention"><option value="1">1 day</option><option value="3" selected>3 days</option><option value="7">7 days</option></select></label><label>Stable for<select id="setup-settle"><option value="30" selected>30 seconds</option><option value="60">1 minute</option><option value="300">5 minutes</option></select></label></div>
        <button class="primary-button full" id="preview-workspace" type="button" ${state.busy ? "disabled" : ""}>${state.busy ? "Building setup…" : "Preview exact setup"}</button>` : ""}
      ${previewStep ? `
        <p>Nothing has moved. Directories go to Kept; loose files go to Inbox before classification.</p>
        <div class="setup-summary"><div><strong>${state.setupPreview!.directories.length}</strong><span>Folders created</span></div><div><strong>${state.setupPreview!.moves.length}</strong><span>Initial moves</span></div></div>
        <div class="setup-moves">${state.setupPreview!.moves.map((move) => `<div><span>${escapeHtml(move.sourcePath)}</span><b>→</b><strong>${escapeHtml(move.destinationPath)}</strong></div>`).join("")}</div>
        <button class="primary-button full" id="apply-workspace" type="button">Apply this setup</button>` : ""}
    </section>
  </dialog>`;
}

function reprocessDialog(): string {
  if (!state.reprocessOpen) return "";
  return `<dialog class="small-dialog" id="reprocess-dialog" open aria-labelledby="reprocess-title">
    <form id="reprocess-form">
      <button class="dialog-close" data-close-reprocess aria-label="Close" type="button">×</button>
      <p class="eyebrow">Reviewed return to Inbox</p><h2 id="reprocess-title">Reprocess files</h2>
      <label class="field"><span>Current area</span><select id="reprocess-area"><option value="library">Library</option><option value="kept">Kept</option></select></label>
      <label class="field"><span>Area-relative paths</span><textarea id="reprocess-paths" placeholder="Work/old-report.pdf&#10;Images/reference.png" required></textarea><small>One file or directory per line. Kept requires explicit paths.</small></label>
      <button class="primary-button full" type="submit">Review reprocessing</button>
    </form>
  </dialog>`;
}

function confirmationDialog(): string {
  const confirmation = state.pendingConfirmation;
  if (!confirmation) return "";
  return `<dialog class="small-dialog confirmation" id="confirmation-dialog" open aria-labelledby="confirmation-title">
    <section>
      <button class="dialog-close" data-cancel-confirmation aria-label="Cancel" type="button">×</button>
      <p class="eyebrow">Filesystem confirmation</p><h2 id="confirmation-title">${escapeHtml(confirmation.title)}</h2><p>${escapeHtml(confirmation.copy)}</p>
      <dl>${confirmation.details.map(([label, value]) => `<div><dt>${escapeHtml(label)}</dt><dd>${escapeHtml(value)}</dd></div>`).join("")}</dl>
      <div class="dialog-actions"><button class="secondary-button" data-cancel-confirmation type="button">Cancel</button><button class="danger-button" id="confirm-action" type="button">${escapeHtml(confirmation.confirmLabel)}</button></div>
    </section>
  </dialog>`;
}

function render(): void {
  app.innerHTML = `<div class="app-shell">
    <header class="topbar">
      <a class="wordmark" href="#" aria-label="Temari home"><span class="wordmark-mark" aria-hidden="true"><i></i><i></i><i></i></span><span>temari</span></a>
      <div class="privacy-note"><span></span>Local-first · No telemetry</div>
    </header>
    <aside class="workspace-rail">
      <div class="rail-heading"><p>Managed folders</p><button data-open-setup type="button" aria-label="Add a folder">+</button></div>
      <nav aria-label="Managed folders">${workspaceNavigation()}</nav>
      <div class="rail-boundary"><span aria-hidden="true">⌾</span><p><strong>Your boundary</strong>Only approved text reaches your configured model.</p></div>
    </aside>
    ${state.busy && !state.status && state.workspaces.length === 0 ? `<main class="loading-state">Loading managed folders…</main>` : dashboard()}
  </div>${setupDialog()}${reprocessDialog()}${confirmationDialog()}`;
  bindEvents();
}

function setBusy(busy: boolean): void {
  state.busy = busy;
  render();
}

async function loadWorkspace(id: string): Promise<void> {
  state.selectedId = id;
  state.notice = null;
  setBusy(true);
  try {
    [state.status, state.schedule, state.history] = await Promise.all([
      getManagedWorkspace(id),
      loadSchedule(id),
      getManagedHistory(id),
    ]);
  } catch (error) {
    state.notice = { tone: "error", message: formatError(error) };
  } finally {
    state.busy = false;
    render();
  }
}

async function refreshSelected(message?: string): Promise<void> {
  if (!state.selectedId) return;
  const id = state.selectedId;
  [state.workspaces, state.status, state.schedule, state.history] = await Promise.all([
    listManagedWorkspaces(),
    getManagedWorkspace(id),
    loadSchedule(id),
    getManagedHistory(id),
  ]);
  if (message) state.notice = { tone: "success", message };
}

function askForConfirmation(confirmation: PendingConfirmation): void {
  state.pendingConfirmation = confirmation;
  render();
}

async function performConfirmation(): Promise<void> {
  const confirmation = state.pendingConfirmation;
  if (!confirmation || state.busy) return;
  state.pendingConfirmation = null;
  setBusy(true);
  try {
    await confirmation.action();
  } catch (error) {
    state.notice = { tone: "error", message: formatError(error) };
  } finally {
    state.busy = false;
    render();
  }
}

function syncProposal(): void {
  if (!state.proposal) return;
  state.proposal.folders = [...document.querySelectorAll<HTMLFieldSetElement>("#setup-folders fieldset")].map((field) => ({
    path: field.querySelector<HTMLInputElement>('input[name="path"]')?.value.trim() ?? "",
    description: field.querySelector<HTMLInputElement>('input[name="description"]')?.value.trim() ?? "",
  }));
}

function bindEvents(): void {
  document.querySelectorAll<HTMLElement>("[data-open-setup]").forEach((button) => button.addEventListener("click", () => {
    state.setupOpen = true;
    state.setupStep = "source";
    state.setupSource = "";
    state.proposal = null;
    state.setupPreview = null;
    render();
  }));
  document.querySelectorAll<HTMLElement>("[data-close-setup]").forEach((button) => button.addEventListener("click", () => {
    state.setupOpen = false;
    render();
  }));
  document.querySelectorAll<HTMLButtonElement>("[data-workspace-id]").forEach((button) => button.addEventListener("click", () => void loadWorkspace(button.dataset.workspaceId!)));

  document.querySelector("#pick-setup-source")?.addEventListener("click", async () => {
    const source = await chooseSource();
    if (source) state.setupSource = source;
    render();
  });
  document.querySelector("#pick-setup-config")?.addEventListener("click", async () => {
    const config = await chooseConfig();
    if (config) state.configPath = config;
    render();
  });
  document.querySelector("#propose-workspace")?.addEventListener("click", async () => {
    setBusy(true);
    try {
      state.proposal = await proposeManagedWorkspace(state.setupSource, state.configPath);
      state.setupStep = "structure";
    } catch (error) {
      state.notice = { tone: "error", message: formatError(error) };
    } finally {
      state.busy = false;
      render();
    }
  });
  document.querySelector("#preview-workspace")?.addEventListener("click", async () => {
    if (!state.proposal) return;
    syncProposal();
    const retentionDays = Number((document.querySelector("#setup-retention") as HTMLSelectElement).value);
    const settleSeconds = Number((document.querySelector("#setup-settle") as HTMLSelectElement).value);
    setBusy(true);
    try {
      state.setupPreview = await previewManagedWorkspace(state.proposal, retentionDays * 86_400, settleSeconds);
      state.setupStep = "preview";
    } catch (error) {
      state.notice = { tone: "error", message: formatError(error) };
    } finally {
      state.busy = false;
      render();
    }
  });
  document.querySelector("#apply-workspace")?.addEventListener("click", () => {
    if (!state.setupPreview) return;
    const preview = state.setupPreview;
    state.setupOpen = false;
    askForConfirmation({
      title: `Set up ${basename(preview.source)}?`,
      copy: "Temari will create Kept, Inbox, and Library, then perform only the moves shown in the reviewed setup.",
      details: [["Folder", preview.source], ["Initial moves", String(preview.moves.length)], ["Directories", String(preview.directories.length)]],
      confirmLabel: "Apply reviewed setup",
      action: async () => {
        const result = await applyManagedWorkspace(preview.token);
        state.workspaces = await listManagedWorkspaces();
        state.selectedId = result.workspace.id;
        state.status = result;
        state.schedule = await loadSchedule(result.workspace.id);
        state.history = [];
        state.notice = { tone: "success", message: "Managed folder created. Its initial moves are journaled." };
      },
    });
  });

  document.querySelector("#run-now")?.addEventListener("click", () => {
    if (!state.status) return;
    const workspace = state.status.workspace;
    askForConfirmation({
      title: `Run ${basename(workspace.source)} now?`,
      copy: "Loose root files will move to Inbox. Eligible Inbox files will move only to approved Library destinations.",
      details: [["Folder", workspace.source], ["Ready now", String(state.status!.inbox.eligibleNow)], ["Collision policy", "Rename safely"]],
      confirmLabel: "Run and apply moves",
      action: async () => {
        const result = await runManagedWorkspace(workspace.id);
        const staged = (result.directoryAdoption?.moveCount ?? 0)
          + result.runs.filter((run) => run.kind === "stage").reduce((total, run) => total + run.moveCount, 0);
        const classified = result.runs.filter((run) => run.kind === "classify").reduce((total, run) => total + run.moveCount, 0);
        await refreshSelected(`${staged} staged and ${classified} classified.`);
      },
    });
  });

  document.querySelector("#save-settings")?.addEventListener("click", async () => {
    if (!state.status) return;
    const enabled = (document.querySelector("#workspace-enabled") as HTMLInputElement).checked;
    setBusy(true);
    try {
      await setManagedWorkspaceEnabled(state.status.workspace.id, enabled);
      state.status = await getManagedWorkspace(state.status.workspace.id);
      state.workspaces = await listManagedWorkspaces();
      state.notice = { tone: "success", message: "Workspace watch state saved." };
    } catch (error) {
      state.notice = { tone: "error", message: formatError(error) };
    } finally { state.busy = false; render(); }
  });

  document.querySelector("#pick-schedule-executable")?.addEventListener("click", async () => {
    try {
      const executable = await chooseTemariExecutable();
      if (executable) state.scheduleExecutablePath = executable;
    } catch (error) {
      state.notice = { tone: "error", message: formatError(error) };
    }
    render();
  });

  document.querySelector("#toggle-schedule")?.addEventListener("click", async () => {
    if (!state.status) return;
    const scheduleOn = state.schedule?.installed && state.schedule.enabled;
    const interval = Number((document.querySelector("#schedule-interval") as HTMLSelectElement).value);
    setBusy(true);
    try {
      state.schedule = scheduleOn
        ? await disableManagedSchedule(state.status.workspace.id)
        : await enableManagedSchedule(state.status.workspace.id, interval, state.scheduleExecutablePath);
      state.notice = { tone: "success", message: scheduleOn ? "Automatic runs turned off." : "Automatic runs turned on." };
    } catch (error) {
      state.notice = { tone: "error", message: formatError(error) };
    } finally { state.busy = false; render(); }
  });

  document.querySelector("#open-reprocess")?.addEventListener("click", () => { state.reprocessOpen = true; render(); });
  document.querySelectorAll("[data-close-reprocess]").forEach((button) => button.addEventListener("click", () => { state.reprocessOpen = false; render(); }));
  document.querySelector("#reprocess-form")?.addEventListener("submit", (event) => {
    event.preventDefault();
    if (!state.status) return;
    const area = (document.querySelector("#reprocess-area") as HTMLSelectElement).value as ReprocessArea;
    const paths = (document.querySelector("#reprocess-paths") as HTMLTextAreaElement).value.split("\n").map((path) => path.trim()).filter(Boolean);
    if (paths.length === 0) return;
    const workspaceId = state.status.workspace.id;
    state.reprocessOpen = false;
    askForConfirmation({
      title: `Return ${paths.length} selection${paths.length === 1 ? "" : "s"} to Inbox?`,
      copy: "This reviewed step does not classify directly from Kept or Library. A later run handles eligible Inbox files.",
      details: [["From", area === "kept" ? "Kept" : "Library"], ["Selections", paths.join(", ")]],
      confirmLabel: "Apply return to Inbox",
      action: async () => {
        const result = await reprocessManagedFiles(workspaceId, area, paths);
        const moved = result.runs.reduce((total, run) => total + run.moveCount, 0);
        await refreshSelected(`${moved} selection${moved === 1 ? "" : "s"} returned to Inbox.`);
      },
    });
  });

  document.querySelectorAll<HTMLButtonElement>("[data-undo-file]").forEach((button) => button.addEventListener("click", () => {
    if (!state.status) return;
    const workspaceId = state.status.workspace.id;
    const runId = button.dataset.runId!;
    const fileId = button.dataset.undoFile!;
    const move = state.history.find((item) => item.sessionId === runId && item.moveId === fileId)!;
    askForConfirmation({
      title: `Undo ${basename(move.destinationPath)}?`,
      copy: "Only this recorded move will be restored. New conflicts are left untouched and reported.",
      details: [["From", move.destinationPath], ["Back to", move.sourcePath], ["Run", runId]],
      confirmLabel: "Undo this move",
      action: async () => { const result = await undoManagedMove(workspaceId, runId, fileId); await refreshSelected(`${result.restoredFiles} move undone.`); },
    });
  }));

  document.querySelectorAll<HTMLButtonElement>("[data-undo-run]").forEach((button) => button.addEventListener("click", () => {
    if (!state.status) return;
    const workspaceId = state.status.workspace.id;
    const runId = button.dataset.undoRun!;
    const moveCount = state.history.filter((move) => move.sessionId === runId && !move.undone).length;
    askForConfirmation({
      title: `Undo run ${runId}?`,
      copy: "Every still-active move from this run will be restored conservatively. New conflicts are left in place.",
      details: [["Run", runId], ["Active moves", String(moveCount)]],
      confirmLabel: "Undo complete run",
      action: async () => { const result = await undoManagedRun(workspaceId, runId); await refreshSelected(`${result.restoredFiles} moves undone.`); },
    });
  }));

  document.querySelectorAll("[data-cancel-confirmation]").forEach((button) => button.addEventListener("click", () => { state.pendingConfirmation = null; render(); }));
  document.querySelector("#confirm-action")?.addEventListener("click", () => void performConfirmation());
}

async function initialize(): Promise<void> {
  try {
    const [location, workspaces] = await Promise.all([defaultConfigLocation(), listManagedWorkspaces()]);
    state.defaultConfigPath = location.defaultPath;
    state.configPath = location.path ?? "";
    state.workspaces = workspaces;
    if (workspaces.length > 0) {
      state.selectedId = workspaces[0].id;
      [state.status, state.schedule, state.history] = await Promise.all([
        getManagedWorkspace(workspaces[0].id),
        loadSchedule(workspaces[0].id),
        getManagedHistory(workspaces[0].id),
      ]);
    }
  } catch (error) {
    state.notice = { tone: "error", message: formatError(error) };
  } finally {
    state.busy = false;
    render();
  }
}

render();
void initialize();
