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
