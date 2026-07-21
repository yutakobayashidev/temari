import "./styles.css";
import { approveStructure, chooseSource, proposeStructure, scanSource } from "./api";
import type { FolderProposal, FolderSet, Proposal, ScanPreview } from "./types";

type Stage = "source" | "scan" | "shape" | "approve";

type AppState = {
  stage: Stage;
  source: string | null;
  scan: ScanPreview | null;
  proposal: Proposal | null;
  approved: FolderSet | null;
  busy: boolean;
  error: string | null;
  configPath: string;
};

const state: AppState = {
  stage: "source",
  source: null,
  scan: null,
  proposal: null,
  approved: null,
  busy: false,
  error: null,
  configPath: ".temari.toml",
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
        <li class="locked"><span>5</span><div><strong>Apply</strong><small>Not in this preview</small></div></li>
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
        <label class="config-strip" id="config-strip" hidden>
          <span>Model config</span>
          <input id="config-path" value=".temari.toml" spellcheck="false" aria-describedby="config-help" />
          <small id="config-help">Used only when requesting a structure</small>
        </label>
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
        <div class="status-message" id="status-message" role="status" aria-live="polite" hidden></div>
        <div class="review-actions">
          <button class="primary-action" id="main-action" type="button" disabled>Select a source</button>
          <button class="apply-action" type="button" disabled title="Apply remains in the audited command-line workflow for this preview">
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
const addFolderButton = $("#add-folder") as HTMLButtonElement;
const configPathInput = $("#config-path") as HTMLInputElement;

function setBusy(busy: boolean): void {
  state.busy = busy;
  mainAction.disabled = busy;
  chooseSourceButton.disabled = busy;
  changeSourceButton.disabled = busy;
  mainAction.classList.toggle("is-busy", busy);
}

function basename(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;
}

function render(): void {
  document.querySelectorAll<HTMLElement>("[data-stage]").forEach((item) => {
    const stages: Stage[] = ["source", "scan", "shape", "approve"];
    const itemStage = item.dataset.stage as Stage;
    item.classList.toggle("active", itemStage === state.stage);
    item.classList.toggle("complete", stages.indexOf(itemStage) < stages.indexOf(state.stage));
  });

  const titles: Record<Stage, [string, string]> = {
    source: ["Start here", "Shape a place for everything."],
    scan: ["Local inventory", "See the loose threads."],
    shape: ["Proposed structure", "Gather files into calm groups."],
    approve: ["Locally approved", "Your structure is ready."],
  };
  $("#stage-kicker").textContent = titles[state.stage][0];
  $("#workspace-title").textContent = titles[state.stage][1];

  const hasSource = state.source !== null;
  ($("#source-strip") as HTMLElement).hidden = !hasSource;
  ($("#center-action") as HTMLElement).hidden = hasSource;
  if (state.source) $("#source-path").textContent = state.source;

  ($("#config-strip") as HTMLElement).hidden = !hasSource;
  const count = state.proposal?.files_considered ?? state.scan?.fileCount;
  const countBox = $("#file-count") as HTMLElement;
  countBox.hidden = count === undefined;
  if (count !== undefined) countBox.querySelector("strong")!.textContent = String(count);

  renderScan();
  renderEditor();
  renderNodes();
  renderReviewText();
  renderAction();
  renderStatus();
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
  nodes.innerHTML = folders
    .map((folder, index) => {
      const angle = -Math.PI / 2 + (index / Math.max(folders.length, 1)) * Math.PI * 2;
      const radius = folders.length > 6 ? 43 : 40;
      const x = 50 + Math.cos(angle) * radius;
      const y = 50 + Math.sin(angle) * radius;
      return `<button class="folder-node" style="--x:${x}%;--y:${y}%;--delay:${index * 45}ms" data-focus-folder="${index}" type="button"><i aria-hidden="true"></i><span>${escapeHtml(folder.path)}</span></button>`;
    })
    .join("");
  $("#thread-stage").classList.toggle("has-proposal", folders.length > 0);
}

function renderReviewText(): void {
  const title = $("#review-title");
  const copy = $("#review-copy");
  if (state.approved) {
    title.textContent = "Structure approved";
    copy.textContent = `${state.approved.folders.length} trusted destinations now have local IDs. No folders or files were changed.`;
  } else if (state.proposal) {
    title.textContent = "Tune the proposal";
    copy.textContent = "Rename, regroup, or remove destinations. Approval validates paths and assigns local destination IDs.";
  } else if (state.scan) {
    title.textContent = "Inventory ready";
    copy.textContent = "Only file names and extensions were inspected. Request a proposal when this scope looks right.";
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
    approve: "Approved locally",
  };
  mainAction.textContent = labels[state.stage];
  mainAction.disabled = state.busy || !state.source || state.approved !== null;
}

function renderStatus(): void {
  const status = $("#status-message") as HTMLElement;
  status.hidden = !state.error && !state.approved;
  status.classList.toggle("error", state.error !== null);
  status.textContent = state.error ?? (state.approved ? "Approval complete. Apply remains locked in this preview." : "");
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
    Object.assign(state, { stage: "source", source, scan: null, proposal: null, approved: null });
  } catch (error) {
    state.error = formatError(error);
  } finally {
    setBusy(false);
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
      state.configPath = configPathInput.value.trim();
      if (!state.configPath) throw new Error("Enter the path to your model configuration file.");
      state.proposal = await proposeStructure(state.source, state.configPath);
      state.stage = "shape";
    } else if (state.stage === "shape" && state.proposal) {
      syncFolderInputs();
      state.approved = await approveStructure(state.proposal);
      state.stage = "approve";
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
  document.querySelector<HTMLInputElement>(`.folder-field[data-folder-index="${index}"] input[name="path"]`)?.focus();
});

render();
