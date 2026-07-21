export type WorkspaceHealth = "healthy" | "disabled" | "attention";

export type ManagedWorkspace = {
  id: string;
  source: string;
  retentionSeconds: number;
  settleSeconds: number;
  enabled: boolean;
  createdUnixMs: number;
  updatedUnixMs: number;
};

export type InboxSummary = {
  physicalFiles: number;
  indexedPending: number;
  indexedPlanned: number;
  indexedMoved: number;
  eligibleNow: number;
  nextEligibleUnixMs: number | null;
};

export type ManagedRun = {
  id: string;
  kind: "setup" | "adopt" | "stage" | "classify";
  state: "planning" | "planned" | "applying" | "completed" | "noop" | "needs_resume" | "failed";
  moveCount: number;
  startedUnixMs: number;
  finishedUnixMs: number | null;
  error: string | null;
};

export type ManagedWorkspaceStatus = {
  health: WorkspaceHealth;
  issues: string[];
  workspace: ManagedWorkspace;
  inbox: InboxSummary;
  runs: {
    total: number;
    actionable: ManagedRun[];
  };
};

export type ManagedMove = {
  sessionId: string;
  kind: "adopt" | "stage" | "classify";
  moveId: string;
  sourcePath: string;
  destinationPath: string;
  undone: boolean;
  undoOutcome: string | null;
  finishedUnixMs: number | null;
};

export type ScheduleStatus = {
  platform: "systemd" | "launchd";
  installed: boolean;
  enabled: boolean;
  active: boolean;
  intervalSeconds: number | null;
};

export type SetupProposal = {
  token: string;
  source: string;
  filesConsidered: number;
  folders: Array<{ path: string; description: string }>;
};

export type SetupPreview = {
  token: string;
  source: string;
  directories: string[];
  moves: Array<{
    sourcePath: string;
    destinationPath: string;
    area: "kept" | "inbox";
  }>;
};

export type ManagedRunResult = {
  workspaceId: string;
  artifactDirectory: string;
  directoryAdoption: null | {
    planPath: string;
    applyPath: string | null;
    moveCount: number;
  };
  runs: ManagedRun[];
};

export type UndoResult = {
  runId: string;
  restoredFiles: number;
  conflicts: number;
  state: "completed" | "partial_failure";
  journalPath: string;
};

export type ReprocessArea = "kept" | "library";
