import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  DefaultSourceLocation,
  LibraryEditOperation,
  LibraryEditPreview,
  LibraryFolder,
  LibraryReorganizationPreview,
  ManagedMove,
  ManagedRunResult,
  ManagedWorkspace,
  ManagedWorkspaceStatus,
  ReprocessArea,
  ScheduleStatus,
  SetupPreview,
  SetupProposal,
  UndoResult,
} from "./types";

export type ConfigLocation = { path: string | null; defaultPath: string };

const now = Date.now();
const demoWorkspaces: ManagedWorkspace[] = [
  {
    id: "workspace-downloads",
    source: "/Users/you/Downloads",
    retentionSeconds: 259_200,
    settleSeconds: 30,
    enabled: true,
    createdUnixMs: now - 12 * 86_400_000,
    updatedUnixMs: now - 18 * 60_000,
  },
  {
    id: "workspace-desktop",
    source: "/Users/you/Desktop",
    retentionSeconds: 259_200,
    settleSeconds: 30,
    enabled: false,
    createdUnixMs: now - 4 * 86_400_000,
    updatedUnixMs: now - 2 * 86_400_000,
  },
];

let demoHistory: ManagedMove[] = [
  {
    sessionId: "run-20260722-01",
    kind: "classify",
    moveId: "f0001",
    sourcePath: "Recents/quarterly-notes.pdf",
    destinationPath: "AI Library/Work/quarterly-notes.pdf",
    undone: false,
    undoOutcome: null,
    finishedUnixMs: now - 18 * 60_000,
  },
  {
    sessionId: "run-20260721-04",
    kind: "stage",
    moveId: "f0002",
    sourcePath: "receipt-july.pdf",
    destinationPath: "Recents/receipt-july.pdf",
    undone: false,
    undoOutcome: null,
    finishedUnixMs: now - 28 * 60 * 60_000,
  },
  {
    sessionId: "run-20260720-02",
    kind: "classify",
    moveId: "f0003",
    sourcePath: "Recents/screenshot-1842.png",
    destinationPath: "AI Library/Images/Screenshots/screenshot-1842.png",
    undone: true,
    undoOutcome: "restored",
    finishedUnixMs: now - 2 * 86_400_000,
  },
];

let demoSchedule: ScheduleStatus = {
  platform: "launchd",
  installed: true,
  enabled: true,
  active: false,
  intervalSeconds: 900,
};
let demoLibraryFolders: LibraryFolder[] = [
  { id: "d000001", path: "Work", description: "Project documents and working material" },
  { id: "d000002", path: "Personal", description: "Personal records and receipts" },
  { id: "d000003", path: "Images", description: "Photos and visual assets" },
];
let demoLibraryPreview: LibraryEditPreview | null = null;
let demoLibraryPreviewWorkspaceId: string | null = null;
let demoLibraryUndoSnapshot: LibraryFolder[] | null = null;
let demoLibraryRedoSnapshot: LibraryFolder[] | null = null;
let demoLatestConfiguration: ManagedWorkspaceStatus["latestConfiguration"] = null;
let demoLatestReorganization: ManagedWorkspaceStatus["latestReorganization"] = null;
let demoLibraryReorganizationPreview: LibraryReorganizationPreview | null = null;
let demoLibraryReorganizationWorkspaceId: string | null = null;

function isTauri(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

function demoWorkspace(id: string): ManagedWorkspace {
  const workspace = demoWorkspaces.find((item) => item.id === id);
  if (!workspace) throw new Error("The selected workspace no longer exists.");
  return workspace;
}

export async function defaultConfigLocation(): Promise<ConfigLocation> {
  if (!isTauri()) {
    const path = "/Users/you/Library/Application Support/dev.yutakobayashidev.temari/config.toml";
    return { path, defaultPath: path };
  }
  return invoke<ConfigLocation>("default_config_location");
}

export async function defaultSourceLocations(): Promise<DefaultSourceLocation[]> {
  if (!isTauri()) {
    return [
      { id: "desktop", label: "Desktop", path: "/Users/you/Desktop" },
      { id: "downloads", label: "Downloads", path: "/Users/you/Downloads" },
      { id: "documents", label: "Documents", path: "/Users/you/Documents" },
    ];
  }
  return invoke<DefaultSourceLocation[]>("default_source_locations");
}

export async function chooseSource(): Promise<string | null> {
  if (!isTauri()) return "/Users/you/Documents";
  const selected = await open({ directory: true, multiple: false, title: "Choose a folder to organize" });
  return typeof selected === "string" ? selected : null;
}

export async function chooseConfig(): Promise<string | null> {
  if (!isTauri()) return "/Users/you/.config/temari/config.toml";
  const selected = await open({
    directory: false,
    multiple: false,
    title: "Choose model configuration",
    filters: [{ name: "TOML configuration", extensions: ["toml"] }],
  });
  return typeof selected === "string" ? selected : null;
}

export async function chooseTemariExecutable(): Promise<string | null> {
  if (!isTauri()) return "/Users/you/.local/bin/temari";
  const selected = await open({
    directory: false,
    multiple: false,
    title: "Choose the Temari CLI executable",
  });
  return typeof selected === "string" ? selected : null;
}

export async function listManagedWorkspaces(): Promise<ManagedWorkspace[]> {
  if (!isTauri()) return structuredClone(demoWorkspaces);
  return invoke<ManagedWorkspace[]>("managed_list_workspaces");
}

export async function getManagedWorkspace(workspaceId: string): Promise<ManagedWorkspaceStatus> {
  if (!isTauri()) {
    const workspace = structuredClone(demoWorkspace(workspaceId));
    return {
      health: workspace.enabled ? "healthy" : "disabled",
      issues: [],
      workspace,
      recents: {
        physicalFiles: workspaceId === "workspace-downloads" ? 7 : 1,
        indexedPending: workspaceId === "workspace-downloads" ? 7 : 1,
        indexedPlanned: 0,
        indexedMoved: 34,
      },
      queue: {
        pendingRuns: 0,
        waitingFiles: workspaceId === "workspace-downloads" ? [{
          relativePath: "Recents/meeting-notes.pdf",
          sizeBytes: 42_000,
          eligibleUnixMs: now + 42 * 60_000,
          reasons: [{ kind: "retention", untilUnixMs: now + 42 * 60_000 }],
        }] : [],
        eligibleFiles: workspaceId === "workspace-downloads" ? 2 : 0,
        nextEligibleUnixMs: workspaceId === "workspace-downloads" ? now + 42 * 60_000 : null,
      },
      activity: { state: "idle", run: null },
      runs: { total: 41, actionable: [] },
      libraryFolders: structuredClone(demoLibraryFolders),
      latestConfiguration: structuredClone(demoLatestConfiguration),
      latestReorganization: structuredClone(demoLatestReorganization),
    };
  }
  return invoke<ManagedWorkspaceStatus>("managed_get_workspace", { request: { workspaceId } });
}

export async function proposeManagedWorkspace(
  source: string,
  configPath: string,
): Promise<SetupProposal> {
  if (!isTauri()) {
    return {
      token: "demo-proposal",
      source,
      filesConsidered: 24,
      folders: [
        { path: "Work", description: "Project documents and working material" },
        { path: "Personal", description: "Personal records, forms, and receipts" },
        { path: "Images", description: "Photos, screenshots, and visual assets" },
      ],
    };
  }
  return invoke<SetupProposal>("managed_propose_workspace", {
    request: { source, configPath, maxFolders: 8 },
  });
}

export async function previewManagedWorkspace(
  proposal: SetupProposal,
  retentionSeconds: number,
  settleSeconds: number,
): Promise<SetupPreview> {
  if (!isTauri()) {
    return {
      token: "demo-preview",
      source: proposal.source,
      directories: ["Manual Library", "Recents", "AI Library", ...proposal.folders.map((folder) => `AI Library/${folder.path}`)],
      moves: [
        { sourcePath: "Existing folder", destinationPath: "Manual Library/Existing folder", area: "manual_library" },
        { sourcePath: "notes.pdf", destinationPath: "Recents/notes.pdf", area: "recents" },
      ],
    };
  }
  return invoke<SetupPreview>("managed_preview_workspace", {
    request: { proposalToken: proposal.token, folders: proposal.folders, retentionSeconds, settleSeconds },
  });
}

export async function applyManagedWorkspace(previewToken: string): Promise<ManagedWorkspaceStatus> {
  if (!isTauri()) {
    const workspace: ManagedWorkspace = {
      id: "workspace-documents",
      source: "/Users/you/Documents",
      retentionSeconds: 259_200,
      settleSeconds: 30,
      enabled: true,
      createdUnixMs: Date.now(),
      updatedUnixMs: Date.now(),
    };
    demoWorkspaces.push(workspace);
    return getManagedWorkspace(workspace.id);
  }
  return invoke<ManagedWorkspaceStatus>("managed_apply_workspace", { request: { previewToken } });
}

export async function setManagedWorkspaceEnabled(workspaceId: string, enabled: boolean): Promise<ManagedWorkspace> {
  if (!isTauri()) {
    Object.assign(demoWorkspace(workspaceId), { enabled, updatedUnixMs: Date.now() });
    return structuredClone(demoWorkspace(workspaceId));
  }
  return invoke<ManagedWorkspace>("managed_set_workspace_enabled", { request: { workspaceId }, enabled });
}

function applyDemoLibraryOperation(folders: LibraryFolder[], operation: LibraryEditPreview["operations"][number]): void {
  if (operation.kind === "add") {
    folders.push({ id: operation.id, path: operation.path, description: operation.description });
    return;
  }
  const index = folders.findIndex((folder) => folder.id === operation.id);
  if (index < 0) throw new Error(`Unknown AI Library destination ${operation.id}.`);
  if (operation.kind === "edit_description") {
    folders[index].description = operation.description;
    return;
  }
  const oldPath = folders[index].path;
  const descendants = folders.filter((folder) => folder.id !== operation.id && folder.path.startsWith(`${oldPath}/`));
  if (descendants.length && operation.descendants === "reject") {
    throw new Error("Choose how nested destinations should be handled.");
  }
  if (operation.descendants === "cascade" && operation.kind === "rename") {
    descendants.forEach((folder) => { folder.path = `${operation.path}${folder.path.slice(oldPath.length)}`; });
  }
  if (operation.descendants === "reparent") {
    const parent = oldPath.includes("/") ? oldPath.slice(0, oldPath.lastIndexOf("/")) : "";
    descendants.forEach((folder) => { folder.path = `${parent ? `${parent}/` : ""}${folder.path.slice(oldPath.length + 1)}`; });
  }
  if (operation.kind === "rename") folders[index].path = operation.path;
  else {
    const removed = new Set([operation.id, ...(operation.descendants === "cascade" ? descendants.map((folder) => folder.id) : [])]);
    for (let position = folders.length - 1; position >= 0; position -= 1) {
      if (removed.has(folders[position].id)) folders.splice(position, 1);
    }
  }
}

export async function previewLibraryEdit(
  workspaceId: string,
  operations: LibraryEditOperation[],
): Promise<LibraryEditPreview> {
  if (!isTauri()) {
    const after = structuredClone(demoLibraryFolders);
    const planned = operations.map((operation, index) => operation.kind === "add"
      ? { ...operation, id: `demo-${Date.now()}-${index}` }
      : operation);
    for (const operation of planned) applyDemoLibraryOperation(after, operation);
    demoLibraryPreview = { token: "demo-library-edit", operations: planned, beforeFolders: structuredClone(demoLibraryFolders), afterFolders: after };
    demoLibraryPreviewWorkspaceId = workspaceId;
    return structuredClone(demoLibraryPreview);
  }
  return invoke<LibraryEditPreview>("managed_preview_library_edit", { request: { workspaceId, operations } });
}

export async function applyLibraryEdit(previewToken: string): Promise<ManagedWorkspaceStatus> {
  if (!isTauri()) {
    if (!demoLibraryPreview || !demoLibraryPreviewWorkspaceId || demoLibraryPreview.token !== previewToken) throw new Error("AI Library edit preview expired.");
    const workspaceId = demoLibraryPreviewWorkspaceId;
    demoLibraryUndoSnapshot = structuredClone(demoLibraryPreview.beforeFolders);
    demoLibraryRedoSnapshot = structuredClone(demoLibraryPreview.afterFolders);
    demoLibraryFolders = structuredClone(demoLibraryPreview.afterFolders);
    demoLatestConfiguration = {
      runId: `demo-config-${Date.now()}`,
      state: "completed",
      undone: false,
      redone: false,
      finishedUnixMs: Date.now(),
    };
    demoLibraryPreview = null;
    demoLibraryPreviewWorkspaceId = null;
    return getManagedWorkspace(workspaceId);
  }
  return invoke<ManagedWorkspaceStatus>("managed_apply_library_edit", { request: { previewToken } });
}

export async function undoLibraryEdit(workspaceId: string, runId: string): Promise<ManagedWorkspaceStatus> {
  if (!isTauri()) {
    if (!demoLatestConfiguration || demoLatestConfiguration.runId !== runId || !demoLibraryUndoSnapshot) {
      throw new Error("AI Library edit can no longer be undone.");
    }
    demoLibraryFolders = structuredClone(demoLibraryUndoSnapshot);
    demoLibraryUndoSnapshot = null;
    demoLatestConfiguration = { ...demoLatestConfiguration, undone: true };
    return getManagedWorkspace(workspaceId);
  }
  return invoke<ManagedWorkspaceStatus>("managed_undo_library_edit", { request: { workspaceId, runId } });
}

export async function redoLibraryEdit(workspaceId: string, runId: string): Promise<ManagedWorkspaceStatus> {
  if (!isTauri()) {
    if (!demoLatestConfiguration || demoLatestConfiguration.runId !== runId || !demoLibraryRedoSnapshot) {
      throw new Error("AI Library edit can no longer be redone.");
    }
    demoLibraryFolders = structuredClone(demoLibraryRedoSnapshot);
    demoLatestConfiguration = { ...demoLatestConfiguration, undone: false, redone: true };
    return getManagedWorkspace(workspaceId);
  }
  return invoke<ManagedWorkspaceStatus>("managed_redo_library_edit", { request: { workspaceId, runId } });
}

export async function resumeLibraryEdit(workspaceId: string, runId: string): Promise<ManagedWorkspaceStatus> {
  if (!isTauri()) return getManagedWorkspace(workspaceId);
  return invoke<ManagedWorkspaceStatus>("managed_resume_library_edit", { request: { workspaceId, runId } });
}

export async function previewLibraryReorganization(
  workspaceId: string,
  configureRunId: string,
): Promise<LibraryReorganizationPreview> {
  if (!isTauri()) {
    demoLibraryReorganizationWorkspaceId = workspaceId;
    demoLibraryReorganizationPreview = {
      token: `demo-library-reorganization-${Date.now()}`,
      directories: ["AI Library/Archive"],
      moves: [{
        sourcePath: "AI Library/Work/quarterly-notes.pdf",
        requestedDestination: "AI Library/Archive/quarterly-notes.pdf",
        destinationPath: "AI Library/Archive/quarterly-notes.pdf",
        target: "approved",
      }],
      attention: [{ sourcePath: "AI Library/Work/edited-locally.txt", reason: "changed" }],
    };
    return structuredClone(demoLibraryReorganizationPreview);
  }
  return invoke<LibraryReorganizationPreview>("managed_preview_library_reorganization", {
    request: { workspaceId, configureRunId },
  });
}

export async function applyLibraryReorganization(previewToken: string): Promise<ManagedWorkspaceStatus> {
  if (!isTauri()) {
    if (!demoLibraryReorganizationPreview || demoLibraryReorganizationPreview.token !== previewToken) {
      throw new Error("AI Library file reorganization preview expired.");
    }
    const configuration = demoLatestConfiguration;
    if (!configuration) throw new Error("No AI Library structure edit is available.");
    const runId = `demo-reorganize-${Date.now()}`;
    for (const [index, move] of demoLibraryReorganizationPreview.moves.entries()) {
      demoHistory.unshift({
        sessionId: runId,
        kind: "reorganize",
        moveId: `r${String(index + 1).padStart(6, "0")}`,
        sourcePath: move.sourcePath,
        destinationPath: move.destinationPath,
        undone: false,
        undoOutcome: null,
        finishedUnixMs: Date.now(),
      });
    }
    demoLatestReorganization = {
      runId,
      configureRunId: configuration.runId,
      state: "completed",
      undone: false,
      moveCount: demoLibraryReorganizationPreview.moves.length,
      finishedUnixMs: Date.now(),
    };
    demoLibraryReorganizationPreview = null;
    const workspaceId = demoLibraryReorganizationWorkspaceId;
    demoLibraryReorganizationWorkspaceId = null;
    if (!workspaceId) throw new Error("AI Library file reorganization preview expired.");
    return getManagedWorkspace(workspaceId);
  }
  return invoke<ManagedWorkspaceStatus>("managed_apply_library_reorganization", { request: { previewToken } });
}

export async function resumeLibraryReorganization(workspaceId: string, runId: string): Promise<ManagedWorkspaceStatus> {
  if (!isTauri()) return getManagedWorkspace(workspaceId);
  return invoke<ManagedWorkspaceStatus>("managed_resume_library_reorganization", { request: { workspaceId, runId } });
}

export async function undoLibraryReorganization(workspaceId: string, runId: string): Promise<ManagedWorkspaceStatus> {
  if (!isTauri()) {
    if (!demoLatestReorganization || demoLatestReorganization.runId !== runId) {
      throw new Error("AI Library file reorganization can no longer be undone.");
    }
    demoLatestReorganization = { ...demoLatestReorganization, undone: true };
    demoHistory = demoHistory.map((move) => move.sessionId === runId
      ? { ...move, undone: true, undoOutcome: "restored" }
      : move);
    return getManagedWorkspace(workspaceId);
  }
  return invoke<ManagedWorkspaceStatus>("managed_undo_library_reorganization", { request: { workspaceId, runId } });
}

export async function runManagedWorkspace(workspaceId: string): Promise<ManagedRunResult> {
  if (!isTauri()) {
    await new Promise((resolve) => window.setTimeout(resolve, 300));
    return { workspaceId, artifactDirectory: "/tmp/temari-demo", directoryAdoption: null, runs: [] };
  }
  return invoke<ManagedRunResult>("managed_run", { request: { workspaceId, apply: true } });
}

export async function reprocessManagedFiles(
  workspaceId: string,
  area: ReprocessArea,
  paths: string[],
): Promise<ManagedRunResult> {
  if (!isTauri()) return {
    workspaceId,
    artifactDirectory: "/tmp/temari-demo",
    directoryAdoption: null,
    runs: [],
  };
  return invoke<ManagedRunResult>("managed_reprocess", {
    request: { workspaceId, area, paths, apply: true },
  });
}

export async function getManagedSchedule(workspaceId: string): Promise<ScheduleStatus> {
  if (!isTauri()) return structuredClone(demoSchedule);
  return invoke<ScheduleStatus>("managed_schedule_status", { request: { workspaceId } });
}

export async function enableManagedSchedule(
  workspaceId: string,
  intervalSeconds: number,
  executablePath: string,
): Promise<ScheduleStatus> {
  if (!isTauri()) {
    demoSchedule = { ...demoSchedule, installed: true, enabled: true, intervalSeconds };
    return structuredClone(demoSchedule);
  }
  return invoke<ScheduleStatus>("managed_schedule_enable", {
    request: { workspaceId, everySeconds: intervalSeconds, executablePath },
  });
}

export async function disableManagedSchedule(workspaceId: string): Promise<ScheduleStatus> {
  if (!isTauri()) {
    demoSchedule = { ...demoSchedule, installed: false, enabled: false, active: false };
    return structuredClone(demoSchedule);
  }
  return invoke<ScheduleStatus>("managed_schedule_disable", { request: { workspaceId } });
}

export async function getManagedHistory(workspaceId: string): Promise<ManagedMove[]> {
  if (!isTauri()) return structuredClone(demoHistory);
  return invoke<ManagedMove[]>("managed_history", { request: { workspaceId, limit: 50 } });
}

export async function undoManagedRun(workspaceId: string, runId: string): Promise<UndoResult> {
  if (!isTauri()) {
    const matches = demoHistory.filter((move) => move.sessionId === runId && !move.undone);
    matches.forEach((move) => { move.undone = true; move.undoOutcome = "restored"; });
    return { runId, restoredFiles: matches.length, conflicts: 0, state: "completed", journalPath: "/tmp/temari-demo-undo.json" };
  }
  return invoke<UndoResult>("managed_undo_session", { request: { workspaceId, sessionId: runId } });
}

export async function undoManagedMove(
  workspaceId: string,
  runId: string,
  fileId: string,
): Promise<UndoResult> {
  if (!isTauri()) {
    const move = demoHistory.find((item) => item.sessionId === runId && item.moveId === fileId && !item.undone);
    if (move) { move.undone = true; move.undoOutcome = "restored"; }
    return { runId, restoredFiles: move ? 1 : 0, conflicts: 0, state: "completed", journalPath: "/tmp/temari-demo-undo.json" };
  }
  return invoke<UndoResult>("managed_undo_move", { request: { workspaceId, sessionId: runId, moveId: fileId } });
}
