import "./styles.css";
import {
  applyLibraryReorganization,
  applyLibraryEdit,
  applyManagedWorkspace,
  chooseConfig,
  chooseSource,
  chooseTemariExecutable,
  defaultConfigLocation,
  defaultSourceLocations,
  disableManagedSchedule,
  enableManagedSchedule,
  getManagedHistory,
  getManagedSchedule,
  getManagedWorkspace,
  listManagedWorkspaces,
  previewManagedWorkspace,
  previewLibraryEdit,
  previewLibraryReorganization,
  proposeManagedWorkspace,
  reprocessManagedFiles,
  redoLibraryEdit,
  runManagedWorkspace,
  setManagedWorkspaceEnabled,
  undoManagedMove,
  undoManagedRun,
  undoLibraryEdit,
  resumeLibraryEdit,
  resumeLibraryReorganization,
  undoLibraryReorganization,
} from "./api";
import type {
  DefaultSourceLocation,
  LibraryEditOperation,
  LibraryEditPreview,
  LibraryReorganizationPreview,
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
  workspaceStatuses: Record<string, ManagedWorkspaceStatus>;
  selectedId: string | null;
  status: ManagedWorkspaceStatus | null;
  schedule: ScheduleStatus | null;
  history: ManagedMove[];
  configPath: string;
  defaultConfigPath: string;
  defaultSources: DefaultSourceLocation[];
  scheduleExecutablePath: string;
  busy: boolean;
  notice: Notice | null;
  setupOpen: boolean;
  setupStep: SetupStep;
  setupSource: string;
  proposal: SetupProposal | null;
  setupPreview: SetupPreview | null;
  reprocessOpen: boolean;
  libraryEditOpen: boolean;
  libraryEditPreview: LibraryEditPreview | null;
  pendingConfirmation: PendingConfirmation | null;
};

const state: AppState = {
  workspaces: [],
  workspaceStatuses: {},
  selectedId: null,
  status: null,
  schedule: null,
  history: [],
  configPath: "",
  defaultConfigPath: "",
  defaultSources: [],
  scheduleExecutablePath: "",
  busy: true,
  notice: null,
  setupOpen: false,
  setupStep: "source",
  setupSource: "",
  proposal: null,
  setupPreview: null,
  reprocessOpen: false,
  libraryEditOpen: false,
  libraryEditPreview: null,
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

function libraryDiffDetails(preview: LibraryEditPreview): Array<[string, string]> {
  const before = new Map(preview.beforeFolders.map((folder) => [folder.id, folder]));
  const after = new Map(preview.afterFolders.map((folder) => [folder.id, folder]));
  const details: Array<[string, string]> = [];
  for (const folder of preview.beforeFolders) {
    const replacement = after.get(folder.id);
    if (!replacement) {
      details.push(["Remove", folder.path]);
    } else {
      if (replacement.path !== folder.path) details.push(["Move path", `${folder.path} → ${replacement.path}`]);
      if (replacement.description !== folder.description) {
        details.push(["Description", `${folder.path}: ${folder.description} → ${replacement.description}`]);
      }
    }
  }
  for (const folder of preview.afterFolders) {
    if (!before.has(folder.id)) details.push(["Add", `${folder.path}: ${folder.description}`]);
  }
  return details;
}

function reorganizationAttentionLabel(reason: LibraryReorganizationPreview["attention"][number]["reason"]): string {
  if (reason === "untracked") return "Not tracked by an earlier classification";
  if (reason === "changed") return "Contents changed after classification";
  if (reason === "unknown_destination") return "Recorded destination is unavailable";
  return "File is outside its recorded destination";
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
      <span class="move-kind">${move.kind === "classify" ? "Classified" : move.kind === "adopt" ? "Manual" : move.kind === "configure" ? "Configured" : move.kind === "reorganize" ? "Reorganized" : "Staged"}</span>
      ${move.undone
        ? `<span class="undo-state">Undone</span>`
        : move.kind === "adopt" || move.kind === "reorganize"
          ? `<span class="undo-state">Undo by run</span>`
          : `<button class="quiet-button" data-undo-file="${escapeAttribute(move.moveId)}" data-run-id="${escapeAttribute(move.sessionId)}" type="button">Undo</button>`}
    </article>`).join("");
}

function workspaceNavigation(): string {
  if (state.workspaces.length === 0) {
    return `<div class="workspace-empty">No folders are managed yet.</div>`;
  }
  return state.workspaces.map((workspace) => {
    const snapshot = state.workspaceStatuses[workspace.id];
    const statusLabel = snapshot?.activity.state === "recoverable" ? "Recovery needed"
      : snapshot?.activity.state === "failed" ? "Failed"
      : snapshot?.queue.eligibleFiles ? `${snapshot.queue.eligibleFiles} ready`
      : snapshot?.queue.waitingFiles.length ? `${snapshot.queue.waitingFiles.length} waiting`
      : workspace.enabled ? "Watching" : "Paused";
    return `
    <button class="workspace-link ${workspace.id === state.selectedId ? "is-selected" : ""}" data-workspace-id="${escapeAttribute(workspace.id)}" type="button">
      <span class="folder-tab" aria-hidden="true"></span>
      <span><strong>${escapeHtml(basename(workspace.source))}</strong><small>${escapeHtml(statusLabel)}</small></span>
      <i class="health-pin ${workspace.enabled ? "" : "is-paused"}" aria-hidden="true"></i>
    </button>`;
  }).join("");
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

  const { workspace, recents, queue, activity } = state.status;
  const classified = state.history.filter((move) => move.kind === "classify" && !move.undone).length;
  const manualNote = "Folders and files you choose to leave alone";
  const recentsNote = queue.nextEligibleUnixMs
    ? `Next review ${formatTime(queue.nextEligibleUnixMs)}`
    : "Nothing is waiting for review";
  const scheduleOn = state.schedule?.installed && state.schedule.enabled;
  const latestRuns = [...new Set(state.history.filter((move) => !move.undone).map((move) => move.sessionId))].slice(0, 3);
  const configuration = state.status.latestConfiguration;
  const reorganization = state.status.latestReorganization;
  const reorganizationMatchesConfiguration = configuration && reorganization?.configureRunId === configuration.runId;
  const activeReorganization = reorganizationMatchesConfiguration && reorganization
    && !reorganization.undone && !["failed", "noop"].includes(reorganization.state);

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
    ${state.status.issues.length ? `<div class="issue-list"><strong>${activity.state === "recoverable" ? "Recovery needed" : activity.state === "failed" ? "Last run failed" : "Needs attention"}</strong>${state.status.issues.map((issue) => `<span>${escapeHtml(issue)}</span>`).join("")}</div>` : ""}

    <section class="areas" aria-labelledby="areas-title">
      <div class="section-heading"><div><p class="eyebrow">Workspace flow</p><h2 id="areas-title">Three places, one clear boundary</h2></div><span>Root → Recents → AI Library</span></div>
      <div class="area-flow">
        <article class="area-card area-manual-library">
          <div class="area-index">M</div>
          <div><p>Leave alone</p><h3>Manual Library</h3><span>${manualNote}</span></div>
          <strong class="area-value">Protected</strong>
        </article>
        <span class="flow-thread" aria-hidden="true"></span>
        <article class="area-card area-recents">
          <div class="area-index">R</div>
          <div><p>Wait here</p><h3>Recents</h3><span>${escapeHtml(recentsNote)}</span></div>
          <strong class="area-value">${recents.physicalFiles}</strong>
          <small class="area-detail">${queue.eligibleFiles} ready · ${queue.waitingFiles.length} waiting</small>
        </article>
        <span class="flow-thread" aria-hidden="true"></span>
        <article class="area-card area-ai-library">
          <div class="area-index">A</div>
          <div><p>Organized by meaning</p><h3>AI Library</h3><span>Approved destinations only</span></div>
          <strong class="area-value">${classified || recents.indexedMoved}</strong>
          <small class="area-detail">recently indexed</small>
        </article>
      </div>
      ${queue.waitingFiles.length ? `<div class="waiting-files">${queue.waitingFiles.slice(0, 5).map((file) => `<div><strong>${escapeHtml(file.relativePath.replace(/^Recents\//, ""))}</strong><span>${file.reasons.map((reason) => reason.kind === "retention" ? `Retention until ${formatTime(reason.untilUnixMs)}` : `Still changing until ${formatTime(reason.untilUnixMs)}`).join(" · ")}</span></div>`).join("")}</div>` : ""}
    </section>

    <section class="library-ledger" aria-labelledby="library-ledger-title">
      <div class="section-heading compact">
        <div><p class="eyebrow">Approved destinations</p><h2 id="library-ledger-title">AI Library structure</h2></div>
        <button class="text-button" id="open-library-editor" type="button" ${workspace.enabled ? "disabled" : ""}>Edit structure</button>
      </div>
      <div class="library-ledger-rows">${state.status.libraryFolders.map((folder) => `<div><strong>${escapeHtml(folder.path)}</strong><span>${escapeHtml(folder.description)}</span></div>`).join("")}</div>
      <p class="field-note">${workspace.enabled ? "Pause this workspace before editing its structure." : "Structure edits do not move existing files. Review existing file moves separately after Apply."}</p>
      ${state.status.latestConfiguration?.state === "completed" && !state.status.latestConfiguration.undone && !state.status.latestConfiguration.redone && !activeReorganization ? `<button class="text-button" id="undo-library-edit" type="button">Undo last structure edit</button>` : ""}
      ${state.status.latestConfiguration?.state === "completed" && state.status.latestConfiguration.undone && !activeReorganization ? `<button class="text-button" id="redo-library-edit" type="button">Redo structure edit</button>` : ""}
      ${state.status.latestConfiguration && ["applying", "needs_resume"].includes(state.status.latestConfiguration.state) ? `<button class="text-button" id="resume-library-edit" type="button">Resume structure edit</button>` : ""}
      ${configuration?.state === "completed" && !configuration.undone && (!reorganizationMatchesConfiguration || reorganization?.undone) ? `<button class="secondary-button library-action" id="preview-library-reorganization" type="button" ${workspace.enabled ? "disabled" : ""}>Review existing file moves</button>` : ""}
      ${reorganizationMatchesConfiguration && reorganization && ["applying", "needs_resume"].includes(reorganization.state) ? `<button class="secondary-button library-action" id="resume-library-reorganization" type="button">Resume file reorganization</button>` : ""}
      ${reorganizationMatchesConfiguration && reorganization?.state === "completed" && !reorganization.undone ? `<button class="text-button" id="undo-library-reorganization" type="button">Undo file reorganization</button>` : ""}
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
          <label class="field"><span>Keep new files in Recents</span><select id="retention-days" disabled>
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
          <div class="control-heading"><div><p class="eyebrow">Send back through Recents</p><h2>Reprocess files</h2></div></div>
          <p class="control-copy">Select files from Manual Library or AI Library. Temari creates a reviewed move back to Recents first.</p>
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
      <h2 id="setup-title">${sourceStep ? "Choose one folder" : structureStep ? "Approve its AI Library" : "Review the exact setup"}</h2>
      ${sourceStep ? `
        <p>Each folder stays independent and gets its own Manual Library, Recents, and AI Library.</p>
        ${state.defaultSources.length ? `<div class="source-suggestions" aria-label="Suggested folders">
          <div><strong>Common places</strong><span>Suggestions only · nothing is added until Apply</span></div>
          <div class="source-suggestion-list">${state.defaultSources.map((source) => `
            <button class="source-suggestion ${state.setupSource === source.path ? "is-selected" : ""}" data-source-path="${escapeAttribute(source.path)}" type="button">
              <span class="source-folder" aria-hidden="true"></span><strong>${escapeHtml(source.label)}</strong><small>${escapeHtml(source.path)}</small>
            </button>`).join("")}</div>
        </div>` : ""}
        <label class="picker-field"><span>Folder</span><div><input id="setup-source" readonly value="${escapeAttribute(state.setupSource)}" placeholder="No folder selected" /><button id="pick-setup-source" type="button">Choose</button></div></label>
        <label class="picker-field"><span>Model configuration</span><div><input id="setup-config" readonly value="${escapeAttribute(state.configPath)}" placeholder="No configuration selected" /><button id="pick-setup-config" type="button">Choose</button></div></label>
        <button class="primary-button full" id="propose-workspace" type="button" ${!state.setupSource || !state.configPath || state.busy ? "disabled" : ""}>${state.busy ? "Reading file names…" : "Propose an AI Library"}</button>` : ""}
      ${structureStep ? `
        <p>${state.proposal!.filesConsidered} file names informed this proposal. Edit every destination before approval.</p>
        <div class="folder-proposal" id="setup-folders">${state.proposal!.folders.map((folder, index) => `
          <fieldset data-folder-index="${index}"><legend>Destination ${index + 1}</legend><input name="path" value="${escapeAttribute(folder.path)}" aria-label="Destination path" /><input name="description" value="${escapeAttribute(folder.description)}" aria-label="Destination purpose" /></fieldset>`).join("")}</div>
        <div class="setup-timing"><label>Recents retention<select id="setup-retention"><option value="1">1 day</option><option value="3" selected>3 days</option><option value="7">7 days</option></select></label><label>Stable for<select id="setup-settle"><option value="30" selected>30 seconds</option><option value="60">1 minute</option><option value="300">5 minutes</option></select></label></div>
        <button class="primary-button full" id="preview-workspace" type="button" ${state.busy ? "disabled" : ""}>${state.busy ? "Building setup…" : "Preview exact setup"}</button>` : ""}
      ${previewStep ? `
        <p>Nothing has moved. Directories go to Manual Library; loose files go to Recents before classification.</p>
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
      <p class="eyebrow">Reviewed return to Recents</p><h2 id="reprocess-title">Reprocess files</h2>
      <label class="field"><span>Current area</span><select id="reprocess-area"><option value="ai_library">AI Library</option><option value="manual_library">Manual Library</option></select></label>
      <label class="field"><span>Area-relative paths</span><textarea id="reprocess-paths" placeholder="Work/old-report.pdf&#10;Images/reference.png" required></textarea><small>One file or directory per line. Manual Library requires explicit paths.</small></label>
      <button class="primary-button full" type="submit">Review reprocessing</button>
    </form>
  </dialog>`;
}

function libraryEditDialog(): string {
  if (!state.libraryEditOpen || !state.status) return "";
  return `<dialog class="sheet-dialog" id="library-edit-dialog" open aria-labelledby="library-edit-title">
    <div class="sheet-backdrop" data-close-library-edit></div>
    <section class="sheet-card library-editor">
      <button class="dialog-close" data-close-library-edit aria-label="Close" type="button">×</button>
      <p class="eyebrow">Approved destinations ledger</p>
      <h2 id="library-edit-title">Edit AI Library structure</h2>
      <p>These changes update future organization only. Existing files stay where they are until you use Reprocess.</p>
      <div class="library-editor-rows">${state.status.libraryFolders.map((folder) => `
        <article data-library-folder="${escapeAttribute(folder.id)}">
          <label><span>Path</span><input name="path" value="${escapeAttribute(folder.path)}" /></label>
          <label class="description"><span>Description</span><input name="description" value="${escapeAttribute(folder.description)}" /></label>
          <label><span>Nested folders</span><select name="descendants"><option value="reject">Reject if present</option><option value="cascade">Move/delete subtree</option><option value="reparent">Keep children at parent</option></select></label>
          <label class="toggle-row"><span><strong>Delete destination</strong><small>Uses the selected nested-folder policy</small></span><input name="delete" type="checkbox" /></label>
        </article>`).join("")}</div>
      <div class="library-add">
        <p class="eyebrow">Add destination</p>
        <input id="library-add-path" placeholder="Work/Reports (optional)" />
        <input id="library-add-description" placeholder="What belongs here" />
      </div>
      <button class="primary-button full" id="review-library-batch" type="button">Review all changes</button>
    </section>
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
  </div>${setupDialog()}${libraryEditDialog()}${reprocessDialog()}${confirmationDialog()}`;
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
    const [status, schedule, history] = await Promise.all([
      getManagedWorkspace(id),
      loadSchedule(id),
      getManagedHistory(id),
    ]);
    if (state.selectedId !== id) return;
    state.status = status;
    state.schedule = schedule;
    state.history = history;
    state.workspaceStatuses[id] = status;
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
  const [workspaces, status, schedule, history] = await Promise.all([
    listManagedWorkspaces(),
    getManagedWorkspace(id),
    loadSchedule(id),
    getManagedHistory(id),
  ]);
  if (state.selectedId !== id) return;
  state.workspaces = workspaces;
  state.status = status;
  state.schedule = schedule;
  state.history = history;
  state.workspaceStatuses[id] = status;
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

async function reviewLibraryEdits(operations: LibraryEditOperation[]): Promise<void> {
  if (!state.status || state.busy) return;
  if (operations.length === 0) {
    state.notice = { tone: "error", message: "Change at least one destination before review." };
    render();
    return;
  }
  setBusy(true);
  try {
    const preview = await previewLibraryEdit(state.status.workspace.id, operations);
    const exactChanges = libraryDiffDetails(preview);
    state.libraryEditPreview = preview;
    state.libraryEditOpen = false;
    askForConfirmation({
      title: `Apply ${operations.length} AI Library structure change${operations.length === 1 ? "" : "s"}?`,
      copy: "Only the approved structure changes. Existing files do not move; use Reprocess when they should be organized again.",
      details: [
        ...exactChanges,
        ["Result", `${preview.beforeFolders.length} → ${preview.afterFolders.length} destinations`],
      ],
      confirmLabel: "Apply structure edit",
      action: async () => {
        await applyLibraryEdit(preview.token);
        state.libraryEditPreview = null;
        await refreshSelected("AI Library structure updated. Existing files were not moved.");
      },
    });
  } catch (error) {
    state.notice = { tone: "error", message: formatError(error) };
  } finally {
    state.busy = false;
    render();
  }
}

async function reviewLibraryReorganization(): Promise<void> {
  const status = state.status;
  const configuration = status?.latestConfiguration;
  if (!status || !configuration || state.busy) return;
  setBusy(true);
  try {
    const preview = await previewLibraryReorganization(status.workspace.id, configuration.runId);
    const details: Array<[string, string]> = [
      ...preview.moves.map((move): [string, string] => ["Move", `${move.sourcePath} → ${move.destinationPath}`]),
      ...preview.moves
        .filter((move) => move.requestedDestination !== move.destinationPath)
        .map((move): [string, string] => ["Collision-safe name", `${move.requestedDestination} → ${move.destinationPath}`]),
      ...preview.attention.map((item): [string, string] => ["Leave in place", `${item.sourcePath} — ${reorganizationAttentionLabel(item.reason)}`]),
    ];
    if (preview.moves.length === 0) {
      askForConfirmation({
        title: "No tracked files need moving",
        copy: preview.attention.length
          ? "Files needing attention remain untouched and are listed below."
          : "The existing AI Library already matches the approved structure.",
        details: details.length ? details : [["Result", "No moves or attention items"]],
        confirmLabel: "Close",
        action: async () => {},
      });
      return;
    }
    askForConfirmation({
      title: `Apply ${preview.moves.length} reviewed file move${preview.moves.length === 1 ? "" : "s"}?`,
      copy: `${preview.attention.length} attention item${preview.attention.length === 1 ? "" : "s"} will remain untouched. Only the exact moves below will be applied.`,
      details,
      confirmLabel: "Apply exact file moves",
      action: async () => {
        await applyLibraryReorganization(preview.token);
        await refreshSelected(`${preview.moves.length} AI Library file move${preview.moves.length === 1 ? "" : "s"} applied.`);
      },
    });
  } catch (error) {
    state.notice = { tone: "error", message: formatError(error) };
  } finally {
    state.busy = false;
    render();
  }
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

  document.querySelector("#open-library-editor")?.addEventListener("click", () => { state.libraryEditOpen = true; render(); });
  document.querySelectorAll("[data-close-library-edit]").forEach((button) => button.addEventListener("click", () => { state.libraryEditOpen = false; render(); }));
  document.querySelector("#review-library-batch")?.addEventListener("click", () => {
    if (!state.status) return;
    const originals = new Map(state.status.libraryFolders.map((folder) => [folder.id, folder]));
    const operations: LibraryEditOperation[] = [];
    document.querySelectorAll<HTMLElement>("[data-library-folder]").forEach((row) => {
      const id = row.dataset.libraryFolder!;
      const original = originals.get(id)!;
      const path = row.querySelector<HTMLInputElement>('input[name="path"]')!.value.trim();
      const description = row.querySelector<HTMLInputElement>('input[name="description"]')!.value.trim();
      const descendants = row.querySelector<HTMLSelectElement>('select[name="descendants"]')!.value as "reject" | "cascade" | "reparent";
      const deleted = row.querySelector<HTMLInputElement>('input[name="delete"]')!.checked;
      if (deleted) operations.push({ kind: "delete", id, descendants });
      else {
        if (path !== original.path) operations.push({ kind: "rename", id, path, descendants });
        if (description !== original.description) operations.push({ kind: "edit_description", id, description });
      }
    });
    const addPath = document.querySelector<HTMLInputElement>("#library-add-path")!.value.trim();
    const addDescription = document.querySelector<HTMLInputElement>("#library-add-description")!.value.trim();
    if (addPath || addDescription) operations.push({ kind: "add", path: addPath, description: addDescription });
    void reviewLibraryEdits(operations);
  });
  document.querySelector("#undo-library-edit")?.addEventListener("click", () => {
    if (!state.status?.latestConfiguration) return;
    const workspaceId = state.status.workspace.id;
    const runId = state.status.latestConfiguration.runId;
    askForConfirmation({
      title: "Undo the last AI Library structure edit?",
      copy: "The previous approved structure returns. Existing files stay in place.",
      details: [["Configure run", runId]],
      confirmLabel: "Undo structure edit",
      action: async () => { await undoLibraryEdit(workspaceId, runId); await refreshSelected("AI Library structure edit undone."); },
    });
  });
  document.querySelector("#redo-library-edit")?.addEventListener("click", () => {
    if (!state.status?.latestConfiguration) return;
    const workspaceId = state.status.workspace.id;
    const runId = state.status.latestConfiguration.runId;
    askForConfirmation({
      title: "Redo the AI Library structure edit?",
      copy: "The reviewed structure returns. Existing files stay in place.",
      details: [["Configure run", runId]],
      confirmLabel: "Redo structure edit",
      action: async () => { await redoLibraryEdit(workspaceId, runId); await refreshSelected("AI Library structure edit redone."); },
    });
  });
  document.querySelector("#resume-library-edit")?.addEventListener("click", async () => {
    if (!state.status?.latestConfiguration) return;
    setBusy(true);
    try {
      await resumeLibraryEdit(state.status.workspace.id, state.status.latestConfiguration.runId);
      await refreshSelected("AI Library structure recovery completed.");
    } catch (error) { state.notice = { tone: "error", message: formatError(error) }; }
    finally { state.busy = false; render(); }
  });
  document.querySelector("#preview-library-reorganization")?.addEventListener("click", () => void reviewLibraryReorganization());
  document.querySelector("#resume-library-reorganization")?.addEventListener("click", async () => {
    const current = state.status?.latestReorganization;
    const workspaceId = state.status?.workspace.id;
    if (!current || !workspaceId) return;
    setBusy(true);
    try {
      await resumeLibraryReorganization(workspaceId, current.runId);
      await refreshSelected("AI Library file reorganization recovery completed.");
    } catch (error) { state.notice = { tone: "error", message: formatError(error) }; }
    finally { state.busy = false; render(); }
  });
  document.querySelector("#undo-library-reorganization")?.addEventListener("click", () => {
    const current = state.status?.latestReorganization;
    const workspaceId = state.status?.workspace.id;
    if (!current || !workspaceId) return;
    askForConfirmation({
      title: `Undo ${current.moveCount} AI Library file move${current.moveCount === 1 ? "" : "s"}?`,
      copy: "Each recorded move is restored conservatively. Files changed after reorganization remain untouched and are reported as conflicts.",
      details: [["Run", current.runId], ["Recorded moves", String(current.moveCount)]],
      confirmLabel: "Undo file reorganization",
      action: async () => {
        await undoLibraryReorganization(workspaceId, current.runId);
        await refreshSelected("AI Library file reorganization undone.");
      },
    });
  });

  document.querySelector("#pick-setup-source")?.addEventListener("click", async () => {
    const source = await chooseSource();
    if (source) state.setupSource = source;
    render();
  });
  document.querySelectorAll<HTMLButtonElement>("[data-source-path]").forEach((button) => button.addEventListener("click", () => {
    state.setupSource = button.dataset.sourcePath ?? "";
    render();
  }));
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
      copy: "Temari will create Manual Library, Recents, and AI Library, then perform only the moves shown in the reviewed setup.",
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
      copy: "Loose root files will move to Recents. Eligible files will move only to approved AI Library destinations.",
      details: [["Folder", workspace.source], ["Ready now", String(state.status!.queue.eligibleFiles)], ["Collision policy", "Rename safely"]],
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
      title: `Return ${paths.length} selection${paths.length === 1 ? "" : "s"} to Recents?`,
      copy: "This reviewed step does not classify directly from Manual Library or AI Library. A later run handles eligible Recents files.",
      details: [["From", area === "manual_library" ? "Manual Library" : "AI Library"], ["Selections", paths.join(", ")]],
      confirmLabel: "Apply return to Recents",
      action: async () => {
        const result = await reprocessManagedFiles(workspaceId, area, paths);
        const moved = result.runs.reduce((total, run) => total + run.moveCount, 0);
        await refreshSelected(`${moved} selection${moved === 1 ? "" : "s"} returned to Recents.`);
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
    const [location, defaultSources, workspaces] = await Promise.all([
      defaultConfigLocation(),
      defaultSourceLocations(),
      listManagedWorkspaces(),
    ]);
    state.defaultConfigPath = location.defaultPath;
    state.configPath = location.path ?? "";
    state.defaultSources = defaultSources;
    state.workspaces = workspaces;
    const statuses = await Promise.all(workspaces.map((workspace) => getManagedWorkspace(workspace.id)));
    state.workspaceStatuses = Object.fromEntries(statuses.map((status) => [status.workspace.id, status]));
    if (workspaces.length > 0) {
      state.selectedId = workspaces[0].id;
      [state.status, state.schedule, state.history] = await Promise.all([
        getManagedWorkspace(workspaces[0].id),
        loadSchedule(workspaces[0].id),
        getManagedHistory(workspaces[0].id),
      ]);
      state.workspaceStatuses[workspaces[0].id] = state.status;
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

function refreshIsPaused(): boolean {
  return state.busy || state.setupOpen || state.reprocessOpen || state.libraryEditOpen || state.pendingConfirmation !== null;
}

async function refreshWorkspaceSnapshots(): Promise<void> {
  const workspaceIds = state.workspaces.map((workspace) => workspace.id);
  const selectedId = state.selectedId;
  const statuses = await Promise.all(workspaceIds.map((id) => getManagedWorkspace(id)));
  for (const status of statuses) state.workspaceStatuses[status.workspace.id] = status;
  if (selectedId && state.selectedId === selectedId) {
    state.status = state.workspaceStatuses[selectedId] ?? state.status;
  }
  render();
}

window.setInterval(() => {
  if (document.visibilityState !== "visible" || refreshIsPaused()) return;
  void refreshWorkspaceSnapshots().catch((error) => {
    state.notice = { tone: "error", message: formatError(error) };
    render();
  });
}, 30_000);

document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible" && !refreshIsPaused()) {
    void refreshWorkspaceSnapshots().catch((error) => {
      state.notice = { tone: "error", message: formatError(error) };
      render();
    });
  }
});
