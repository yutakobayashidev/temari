import "./styles.css";
import { approveStructure, chooseConfig, chooseSource, defaultConfigLocation, previewPlan, proposeStructure, scanSource } from "./api";
import type { ClassificationBasis, FolderProposal, FolderSet, PlanPreview, Proposal, ScanPreview } from "./types";

type Stage = "source" | "scan" | "shape" | "approve" | "plan";

type AppState = {
  stage: Stage;
  source: string | null;
  scan: ScanPreview | null;
  proposal: Proposal | null;
  approved: FolderSet | null;
  planPreview: PlanPreview | null;
  busy: boolean;
  error: string | null;
  configPath: string;
  defaultConfigPath: string;
};

const state: AppState = {
  stage: "source",
  source: null,
  scan: null,
  proposal: null,
  approved: null,
  planPreview: null,
  busy: false,
  error: null,
  configPath: "",
  defaultConfigPath: "",
};

const app = document.querySelector<HTMLDivElement>("#app");
if (!app) throw new Error("App root not found");

app.innerHTML = `
  <div class="shell">
    <header class="topbar">
      <a class="wordmark" href="#" aria-label="Temari home">
        <span class="wordmark-mark" aria-hidden="true"><i></i><i></i><i></i></span>
        <span>temari</span>
      </a>
      <div class="privacy-note"><span class="privacy-dot" aria-hidden="true"></span> Local-first session</div>
    </header>

    <aside class="stage-rail" aria-label="Organization stages">
      <p class="rail-label">Workflow</p>
      <ol>
        <li data-stage="source"><span>1</span><div><strong>Source</strong><small>Choose a folder</small></div></li>
        <li data-stage="scan"><span>2</span><div><strong>Scan</strong><small>Read names locally</small></div></li>
        <li data-stage="shape"><span>3</span><div><strong>Shape</strong><small>Review the structure</small></div></li>
        <li data-stage="approve"><span>4</span><div><strong>Approve</strong><small>Trust the destinations</small></div></li>
        <li data-stage="plan"><span>5</span><div><strong>Plan</strong><small>Review every move</small></div></li>
        <li class="locked"><span>6</span><div><strong>Apply</strong><small>Not in this preview</small></div></li>
      </ol>
      <div class="local-boundary">
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 2 4.5 5.2v5.9c0 4.9 3.1 9.4 7.5 10.9 4.4-1.5 7.5-6 7.5-10.9V5.2L12 2Zm0 3.1 4.5 1.9v4.1c0 3.4-1.8 6.6-4.5 7.8-2.7-1.2-4.5-4.4-4.5-7.8V7L12 5.1Z"/></svg>
        <p><strong>Your boundary</strong>Names stay local until you request a proposal.</p>
      </div>
    </aside>

    <main class="workspace">
      <section class="canvas" aria-labelledby="workspace-title">
        <div class="canvas-heading">
          <div>
            <p class="eyebrow" id="stage-kicker">Start here</p>
            <h1 id="workspace-title">Shape a place for everything.</h1>
          </div>
          <div class="file-count" id="file-count" hidden><strong>0</strong><span>files considered</span></div>
        </div>
        <div class="thread-stage" id="thread-stage">
          <svg class="thread-orbit" viewBox="0 0 640 560" role="img" aria-label="Proposed folder structure">
            <defs>
              <filter id="soft"><feGaussianBlur stdDeviation="0.7" /></filter>
            </defs>
            <circle class="orbit orbit-a" cx="320" cy="280" r="198" />
            <circle class="orbit orbit-b" cx="320" cy="280" r="145" />
            <ellipse class="thread thread-a" cx="320" cy="280" rx="220" ry="100" transform="rotate(24 320 280)" />
            <ellipse class="thread thread-b" cx="320" cy="280" rx="214" ry="112" transform="rotate(-31 320 280)" />
            <ellipse class="thread thread-c" cx="320" cy="280" rx="114" ry="222" transform="rotate(61 320 280)" />
          </svg>
          <div class="center-action" id="center-action">
            <button class="source-button" id="choose-source" type="button">
              <span class="button-icon" aria-hidden="true">⌁</span>
              <span><strong>Choose a folder</strong><small>Nothing changes on disk</small></span>
            </button>
          </div>
          <div class="folder-nodes" id="folder-nodes"></div>
        </div>
        <div class="source-strip" id="source-strip" hidden>
          <span class="source-icon" aria-hidden="true">⌂</span>
          <span><small>Source</small><strong id="source-path"></strong></span>
          <button id="change-source" class="text-button" type="button">Change</button>
        </div>
        <div class="config-strip" id="config-strip" hidden>
          <label for="config-path">Model config</label>
          <input id="config-path" readonly placeholder="No configuration selected" aria-describedby="config-help" />
          <button id="choose-config" class="text-button" type="button">Choose</button>
          <small id="config-help">Select the TOML file used for model access.</small>
        </div>
      </section>

      <aside class="review" aria-labelledby="review-title">
        <div class="review-header">
          <p class="eyebrow">Review panel</p>
          <h2 id="review-title">No proposal yet</h2>
          <p id="review-copy">Choose a folder, inspect the local scan, then ask your configured model for a structure.</p>
        </div>
        <div class="scan-summary" id="scan-summary" hidden></div>
        <form class="folder-editor" id="folder-editor" hidden>
          <div class="editor-heading"><span>Destinations</span><button class="text-button" id="add-folder" type="button">+ Add folder</button></div>
          <div id="folder-fields"></div>
        </form>
        <section class="plan-preview" id="plan-preview" aria-label="Move plan" hidden>
          <div class="plan-summary" id="plan-summary"></div>
          <div class="plan-entries" id="plan-entries"></div>
          <details class="directory-details" id="directory-details">
            <summary>Folders to create</summary>
            <ul id="directory-list"></ul>
          </details>
        </section>
        <div class="status-message" id="status-message" role="status" aria-live="polite" hidden></div>
        <div class="review-actions">
          <button class="primary-action" id="main-action" type="button" disabled>Select a source</button>
          <button class="apply-action" id="apply-action" type="button" disabled title="Apply remains in the audited command-line workflow for this preview">
            <span>Apply moves</span><small>Locked in this preview</small>
          </button>
        </div>
      </aside>
    </main>
  </div>
`;

const $ = <T extends Element>(selector: string): T => {
  const element = document.querySelector<T>(selector);
  if (!element) throw new Error(`Missing element: ${selector}`);
  return element;
};

const mainAction = $("#main-action") as HTMLButtonElement;
const chooseSourceButton = $("#choose-source") as HTMLButtonElement;
const changeSourceButton = $("#change-source") as HTMLButtonElement;
const chooseConfigButton = $("#choose-config") as HTMLButtonElement;
const addFolderButton = $("#add-folder") as HTMLButtonElement;
const configPathInput = $("#config-path") as HTMLInputElement;

function setBusy(busy: boolean): void {
  state.busy = busy;
  mainAction.disabled = busy;
  chooseSourceButton.disabled = busy;
  changeSourceButton.disabled = busy;
  chooseConfigButton.disabled = busy;
  mainAction.classList.toggle("is-busy", busy);
}

function basename(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;
}

function render(): void {
  document.querySelectorAll<HTMLElement>("[data-stage]").forEach((item) => {
    const stages: Stage[] = ["source", "scan", "shape", "approve", "plan"];
    const itemStage = item.dataset.stage as Stage;
    item.classList.toggle("active", itemStage === state.stage);
    item.classList.toggle("complete", stages.indexOf(itemStage) < stages.indexOf(state.stage));
  });

  const titles: Record<Stage, [string, string]> = {
    source: ["Start here", "Shape a place for everything."],
    scan: ["Local inventory", "See the loose threads."],
    shape: ["Proposed structure", "Gather files into calm groups."],
    approve: ["Locally approved", "Your structure is ready."],
    plan: ["Read-only plan", "Follow every thread before it moves."],
  };
  $("#stage-kicker").textContent = titles[state.stage][0];
  $("#workspace-title").textContent = titles[state.stage][1];

  const hasSource = state.source !== null;
  ($("#source-strip") as HTMLElement).hidden = !hasSource;
  ($("#center-action") as HTMLElement).hidden = hasSource;
  if (state.source) $("#source-path").textContent = state.source;

  ($("#config-strip") as HTMLElement).hidden = !hasSource;
  configPathInput.value = state.configPath;
  configPathInput.title = state.configPath;
  chooseConfigButton.textContent = state.configPath ? "Change" : "Choose";
  $("#config-help").textContent = state.configPath
    ? `Loaded for this session: ${state.configPath}`
    : state.defaultConfigPath
      ? `No config found at ${state.defaultConfigPath}`
      : "Checking the standard config location…";
  const count = state.proposal?.files_considered ?? state.scan?.fileCount;
  const countBox = $("#file-count") as HTMLElement;
  countBox.hidden = count === undefined;
  if (count !== undefined) countBox.querySelector("strong")!.textContent = String(count);

  renderScan();
  renderEditor();
  renderPlan();
  renderNodes();
  renderReviewText();
  renderAction();
  renderStatus();
}

function renderPlan(): void {
  const preview = $("#plan-preview") as HTMLElement;
  preview.hidden = state.planPreview === null;
  if (!state.planPreview) return;

  const { plan, sha256 } = state.planPreview;
  const moveCount = plan.entries.length;
  $("#plan-summary").innerHTML = `
    <div><strong>${moveCount}</strong><span>Moves</span></div>
    <div><strong>${plan.directories.length}</strong><span>New folders</span></div>
    <div><strong>Safe</strong><span>Rename collisions</span></div>`;

  const entries = $("#plan-entries");
  if (moveCount === 0) {
    entries.innerHTML = `<div class="plan-empty"><strong>No moves needed</strong><span>Every in-scope file is already inside an approved destination.</span></div>`;
  } else {
    entries.innerHTML = plan.entries
      .map(
        (entry) => `
          <article class="plan-entry" data-destination-id="${escapeAttribute(entry.destination_id)}" tabindex="-1">
            <div class="move-path source-move"><span>From</span><strong>${escapeHtml(entry.source_path)}</strong></div>
            <span class="move-arrow" aria-hidden="true">↓</span>
            <div class="move-path destination-move"><span>To</span><strong>${escapeHtml(entry.destination_path)}</strong></div>
            <span class="basis-chip">${basisLabel(entry.classification_basis)}</span>
          </article>`,
      )
      .join("");
  }

  const details = $("#directory-details") as HTMLDetailsElement;
  details.hidden = plan.directories.length === 0;
  details.querySelector("summary")!.textContent = `Folders to create (${plan.directories.length})`;
  $("#directory-list").innerHTML = plan.directories.map((path) => `<li>${escapeHtml(path)}</li>`).join("");
  details.title = `Plan ${sha256}`;
}

function basisLabel(basis: ClassificationBasis): string {
  const labels: Record<ClassificationBasis, string> = {
    name: "Name",
    content: "Content",
    extension_fallback: "Fallback",
    rule: "Rule",
  };
  return labels[basis];
}

function renderScan(): void {
  const summary = $("#scan-summary") as HTMLElement;
  summary.hidden = state.scan === null || state.proposal !== null;
  if (!state.scan) return;
  const extensions = Object.entries(state.scan.extensionCounts)
    .sort((a, b) => b[1] - a[1])
    .slice(0, 6)
    .map(([extension, count]) => `<li><span>.${escapeHtml(extension)}</span><strong>${count}</strong></li>`)
    .join("");
  summary.innerHTML = `<div class="scan-total"><strong>${state.scan.fileCount}</strong><span>files found in ${escapeHtml(basename(state.scan.source))}</span></div><ul>${extensions}</ul>`;
}

function renderEditor(): void {
  const editor = $("#folder-editor") as HTMLElement;
  editor.hidden = state.proposal === null || state.approved !== null;
  if (!state.proposal || state.approved) return;
  $("#folder-fields").innerHTML = state.proposal.folders
    .map(
      (folder, index) => `
        <fieldset class="folder-field" data-folder-index="${index}">
          <legend>Destination ${index + 1}</legend>
          <span class="folder-glyph" aria-hidden="true"></span>
          <label><span>Path</span><input name="path" value="${escapeAttribute(folder.path)}" aria-label="Destination ${index + 1} path" /></label>
          <label><span>Purpose</span><input name="description" value="${escapeAttribute(folder.description)}" aria-label="Destination ${index + 1} purpose" /></label>
          <button class="remove-folder" type="button" aria-label="Remove ${escapeAttribute(folder.path)}">×</button>
        </fieldset>`,
    )
    .join("");
}

function renderNodes(): void {
  const nodes = $("#folder-nodes");
  const folders = state.proposal?.folders ?? [];
  const approvedByPath = new Map(state.approved?.folders.map((folder) => [folder.path, folder.id]) ?? []);
  const moveCounts = new Map<string, number>();
  for (const entry of state.planPreview?.plan.entries ?? []) {
    moveCounts.set(entry.destination_id, (moveCounts.get(entry.destination_id) ?? 0) + 1);
  }
  nodes.innerHTML = folders
    .map((folder, index) => {
      const angle = -Math.PI / 2 + (index / Math.max(folders.length, 1)) * Math.PI * 2;
      const radius = folders.length > 6 ? 43 : 40;
      const x = 50 + Math.cos(angle) * radius;
      const y = 50 + Math.sin(angle) * radius;
      const destinationId = approvedByPath.get(folder.path);
      const count = destinationId ? moveCounts.get(destinationId) : undefined;
      const badge = count === undefined ? "" : `<b aria-label="${count} moves">${count}</b>`;
      return `<button class="folder-node" style="--x:${x}%;--y:${y}%;--delay:${index * 45}ms" data-focus-folder="${index}"${destinationId ? ` data-destination-id="${escapeAttribute(destinationId)}"` : ""} type="button"><i aria-hidden="true"></i><span>${escapeHtml(folder.path)}</span>${badge}</button>`;
    })
    .join("");
  $("#thread-stage").classList.toggle("has-proposal", folders.length > 0);
}

function renderReviewText(): void {
  const title = $("#review-title");
  const copy = $("#review-copy");
  if (state.approved) {
    if (state.planPreview) {
      if (state.planPreview.plan.entries.length === 0) {
        title.textContent = "No moves needed";
        copy.textContent = "Every in-scope file is already inside an approved destination.";
      } else {
        title.textContent = "Review every move";
        copy.textContent = "This plan is read-only. Check each source and destination before files can change.";
      }
    } else {
      title.textContent = "Destinations approved";
      copy.textContent = "The trusted destinations are ready. Preview the exact file moves next.";
    }
  } else if (state.proposal) {
    title.textContent = "Tune the proposal";
    copy.textContent = "Rename, regroup, or remove destinations. Approval validates paths and assigns local destination IDs.";
  } else if (state.scan) {
    title.textContent = state.configPath ? "Inventory ready" : "Choose model access";
    copy.textContent = state.configPath
      ? "Only file names and extensions were inspected. Request a proposal when this scope looks right."
      : "Choose the TOML configuration used to request a folder structure.";
  } else if (state.source) {
    title.textContent = "Ready to scan";
    copy.textContent = "The scan is read-only and stays at the top level for this preview.";
  } else {
    title.textContent = "No proposal yet";
    copy.textContent = "Choose a folder, inspect the local scan, then ask your configured model for a structure.";
  }
}

function renderAction(): void {
  const labels: Record<Stage, string> = {
    source: state.source ? "Scan this folder" : "Select a source",
    scan: "Request a structure",
    shape: "Approve destinations",
    approve: "Preview moves",
    plan: "Plan ready",
  };
  mainAction.textContent = labels[state.stage];
  mainAction.disabled = state.busy
    || !state.source
    || state.stage === "plan"
    || (state.stage === "scan" && !state.configPath);
  const applyAction = $("#apply-action") as HTMLButtonElement;
  const moveCount = state.planPreview?.plan.entries.length;
  applyAction.querySelector("span")!.textContent = moveCount === undefined ? "Apply moves" : `Apply ${moveCount} moves`;
}

function renderStatus(): void {
  const status = $("#status-message") as HTMLElement;
  status.hidden = !state.error && !state.approved;
  status.classList.toggle("error", state.error !== null);
  status.textContent = state.error ?? (state.planPreview
    ? `Plan ready. ${state.planPreview.plan.entries.length} moves and ${state.planPreview.plan.directories.length} folders to create. Nothing changed on disk.`
    : state.approved
      ? "Destinations approved. Preview the moves before any files can change."
      : "");
}

function escapeHtml(value: string): string {
  const node = document.createElement("span");
  node.textContent = value;
  return node.innerHTML;
}

function escapeAttribute(value: string): string {
  return escapeHtml(value).replaceAll('"', "&quot;");
}

async function selectSource(): Promise<void> {
  setBusy(true);
  state.error = null;
  try {
    const source = await chooseSource();
    if (!source) return;
    Object.assign(state, { stage: "source", source, scan: null, proposal: null, approved: null, planPreview: null });
  } catch (error) {
    state.error = formatError(error);
  } finally {
    setBusy(false);
    render();
  }
}

async function selectConfig(): Promise<void> {
  if (state.busy) return;
  setBusy(true);
  state.error = null;
  try {
    const configPath = await chooseConfig();
    if (configPath) state.configPath = configPath;
  } catch (error) {
    state.error = formatError(error);
  } finally {
    setBusy(false);
    render();
  }
}

async function initializeConfig(): Promise<void> {
  try {
    const location = await defaultConfigLocation();
    state.defaultConfigPath = location.defaultPath;
    if (!state.configPath && location.path) state.configPath = location.path;
  } catch (error) {
    state.error = formatError(error);
  } finally {
    render();
  }
}

async function advance(): Promise<void> {
  if (!state.source || state.busy) return;
  setBusy(true);
  state.error = null;
  try {
    if (state.stage === "source") {
      state.scan = await scanSource(state.source);
      state.stage = "scan";
    } else if (state.stage === "scan") {
      if (!state.configPath) throw new Error("Choose the model configuration file in the app.");
      state.proposal = await proposeStructure(state.source, state.configPath);
      state.stage = "shape";
    } else if (state.stage === "shape" && state.proposal) {
      syncFolderInputs();
      state.approved = await approveStructure(state.proposal);
      state.stage = "approve";
    } else if (state.stage === "approve" && state.approved) {
      state.planPreview = await previewPlan();
      state.stage = "plan";
    }
  } catch (error) {
    state.error = formatError(error);
  } finally {
    setBusy(false);
    render();
  }
}

function syncFolderInputs(): void {
  if (!state.proposal) return;
  const folders: FolderProposal[] = [];
  document.querySelectorAll<HTMLFieldSetElement>(".folder-field").forEach((field) => {
    const path = field.querySelector<HTMLInputElement>('input[name="path"]')?.value.trim() ?? "";
    const description = field.querySelector<HTMLInputElement>('input[name="description"]')?.value.trim() ?? "";
    folders.push({ path, description });
  });
  state.proposal.folders = folders;
}

function formatError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

chooseSourceButton.addEventListener("click", selectSource);
changeSourceButton.addEventListener("click", selectSource);
chooseConfigButton.addEventListener("click", selectConfig);
mainAction.addEventListener("click", advance);

addFolderButton.addEventListener("click", () => {
  if (!state.proposal) return;
  syncFolderInputs();
  state.proposal.folders.push({ path: "New folder", description: "Describe what belongs here" });
  render();
  document.querySelector<HTMLInputElement>('.folder-field:last-child input[name="path"]')?.select();
});

$("#folder-fields").addEventListener("input", () => {
  syncFolderInputs();
  renderNodes();
});

$("#folder-fields").addEventListener("click", (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>(".remove-folder");
  if (!button || !state.proposal) return;
  const field = button.closest<HTMLFieldSetElement>(".folder-field");
  const index = Number(field?.dataset.folderIndex);
  syncFolderInputs();
  state.proposal.folders.splice(index, 1);
  render();
});

$("#folder-nodes").addEventListener("click", (event) => {
  const node = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-focus-folder]");
  if (!node) return;
  const index = Number(node.dataset.focusFolder);
  if (state.planPreview && node.dataset.destinationId) {
    document.querySelector<HTMLElement>(`.plan-entry[data-destination-id="${CSS.escape(node.dataset.destinationId)}"]`)?.focus();
  } else {
    document.querySelector<HTMLInputElement>(`.folder-field[data-folder-index="${index}"] input[name="path"]`)?.focus();
  }
});

render();
void initializeConfig();
