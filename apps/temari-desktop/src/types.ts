export type FolderProposal = {
  path: string;
  description: string;
};

export type Proposal = {
  version: number;
  source: string;
  scope: { recursive_roots: string[] };
  files_considered: number;
  folders: FolderProposal[];
};

export type ScanPreview = {
  source: string;
  scope: { recursive_roots: string[] };
  fileCount: number;
  sampledFiles: Array<{ id: string; source_path: string; extension: string }>;
  extensionCounts: Record<string, number>;
};

export type ApprovedFolder = FolderProposal & {
  id: string;
  model_visible: boolean;
  fallback: string | null;
};

export type FolderSet = {
  version: number;
  source: string;
  scope: { recursive_roots: string[] };
  folders: ApprovedFolder[];
};

export type ClassificationBasis = "name" | "content" | "extension_fallback" | "rule";

export type FsIdentity = {
  device: number;
  inode: number;
};

export type FileFingerprint = {
  identity: FsIdentity;
  size: number;
  sha256: string;
};

export type PlanEntry = {
  file_id: string;
  source_path: string;
  source_fingerprint: FileFingerprint;
  destination_id: string;
  requested_destination: string;
  destination_path: string;
  reasoning: string | null;
  classification_basis: ClassificationBasis;
  rule_id: string | null;
};

export type Plan = {
  version: number;
  source: string;
  source_identity: FsIdentity;
  scope: { recursive_roots: string[] };
  collision_policy: "rename";
  folders: ApprovedFolder[];
  directories: string[];
  entries: PlanEntry[];
};

export type PlanPreview = {
  plan: Plan;
  sha256: string;
};

export type ApplyResult = {
  state: "running" | "completed" | "failed" | "partial_failure";
  sessionId: string;
  planSha256: string;
  plannedFiles: number;
  movedFiles: number;
  createdDirectories: number;
  conflicts: number;
  runDirectory: string;
  planPath: string;
  journalPath: string;
};

export type UndoResult = {
  state: "running" | "completed" | "partial_failure";
  applySessionId: string;
  restoredFiles: number;
  removedDirectories: number;
  conflicts: number;
  journalPath: string;
};
