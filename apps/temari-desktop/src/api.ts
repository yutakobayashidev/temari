import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  DefaultSourceLocation,
  LibraryEditOperation,
  LibraryEditPreview,
  LibraryFolder,
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
let demoLatestConfiguration: ManagedWorkspaceStatus["latestConfiguration"] = null;

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
      inbox: {
        physicalFiles: workspaceId === "workspace-downloads" ? 7 : 1,
        indexedPending: workspaceId === "workspace-downloads" ? 7 : 1,
        indexedPlanned: 0,
        indexedMoved: 34,
        eligibleNow: workspaceId === "workspace-downloads" ? 2 : 0,
        nextEligibleUnixMs: now + 42 * 60_000,
      },
      runs: { total: 41, actionable: [] },
      libraryFolders: structuredClone(demoLibraryFolders),
      latestConfiguration: structuredClone(demoLatestConfiguration),
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
        { sourcePath: "Existing folder", destinationPath: "Manual Library/Existing folder", area: "kept" },
        { sourcePath: "notes.pdf", destinationPath: "Recents/notes.pdf", area: "inbox" },
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

export async function previewLibraryEdit(
  workspaceId: string,
  operation: LibraryEditOperation,
): Promise<LibraryEditPreview> {
  if (!isTauri()) {
    const after = structuredClone(demoLibraryFolders);
    if (operation.kind === "add") after.push({ id: `demo-${Date.now()}`, path: operation.path, description: operation.description });
    if (operation.kind === "rename") after.find((folder) => folder.id === operation.id)!.path = operation.path;
    if (operation.kind === "edit_description") after.find((folder) => folder.id === operation.id)!.description = operation.description;
    if (operation.kind === "delete") after.splice(after.findIndex((folder) => folder.id === operation.id), 1);
    demoLibraryPreview = { token: "demo-library-edit", operation, beforeFolders: structuredClone(demoLibraryFolders), afterFolders: after };
    demoLibraryPreviewWorkspaceId = workspaceId;
    return structuredClone(demoLibraryPreview);
  }
  return invoke<LibraryEditPreview>("managed_preview_library_edit", { request: { workspaceId, operation } });
}

export async function applyLibraryEdit(previewToken: string): Promise<ManagedWorkspaceStatus> {
  if (!isTauri()) {
    if (!demoLibraryPreview || !demoLibraryPreviewWorkspaceId || demoLibraryPreview.token !== previewToken) throw new Error("Library edit preview expired.");
    const workspaceId = demoLibraryPreviewWorkspaceId;
    demoLibraryUndoSnapshot = structuredClone(demoLibraryPreview.beforeFolders);
    demoLibraryFolders = structuredClone(demoLibraryPreview.afterFolders);
    demoLatestConfiguration = {
      runId: `demo-config-${Date.now()}`,
      state: "completed",
      undone: false,
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
      throw new Error("Library edit can no longer be undone.");
    }
    demoLibraryFolders = structuredClone(demoLibraryUndoSnapshot);
    demoLibraryUndoSnapshot = null;
    demoLatestConfiguration = { ...demoLatestConfiguration, undone: true };
    return getManagedWorkspace(workspaceId);
  }
  return invoke<ManagedWorkspaceStatus>("managed_undo_library_edit", { request: { workspaceId, runId } });
}

export async function resumeLibraryEdit(workspaceId: string, runId: string): Promise<ManagedWorkspaceStatus> {
  if (!isTauri()) return getManagedWorkspace(workspaceId);
  return invoke<ManagedWorkspaceStatus>("managed_resume_library_edit", { request: { workspaceId, runId } });
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
