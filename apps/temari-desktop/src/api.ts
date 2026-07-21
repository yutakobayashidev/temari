import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { FolderSet, Proposal, ScanPreview } from "./types";

const demoProposal: Proposal = {
  version: 1,
  source: "/Users/you/Downloads",
  scope: { recursive_roots: [] },
  files_considered: 18,
  folders: [
    { path: "Documents/Work", description: "Project notes, briefs, and reference documents" },
    { path: "Documents/Personal", description: "Receipts, forms, and personal records" },
    { path: "Media/Images", description: "Photos, screenshots, and visual assets" },
    { path: "Media/Audio", description: "Music, recordings, and audio exports" },
    { path: "Packages", description: "Archives, installers, and disk images" },
  ],
};

function isTauri(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

export async function chooseSource(): Promise<string | null> {
  if (!isTauri()) return demoProposal.source;
  const selected = await open({
    directory: true,
    multiple: false,
    title: "Choose a folder to organize",
  });
  return typeof selected === "string" ? selected : null;
}

export async function scanSource(source: string): Promise<ScanPreview> {
  if (!isTauri()) {
    return {
      source,
      scope: { recursive_roots: [] },
      fileCount: demoProposal.files_considered,
      sampledFiles: [],
      extensionCounts: { pdf: 5, png: 4, zip: 3, md: 2, mp3: 2, other: 2 },
    };
  }
  return invoke<ScanPreview>("scan_source", { request: { source, recursiveRoots: [] } });
}

export async function proposeStructure(source: string, configPath: string): Promise<Proposal> {
  if (!isTauri()) return { ...demoProposal, source };
  return invoke<Proposal>("propose_structure", {
    request: { configPath, source, recursiveRoots: [], maxFolders: 8 },
  });
}

export async function approveStructure(proposal: Proposal): Promise<FolderSet> {
  if (!isTauri()) {
    return {
      version: 1,
      source: proposal.source,
      scope: proposal.scope,
      folders: proposal.folders.map((folder, index) => ({
        ...folder,
        id: `d${String(index + 1).padStart(4, "0")}`,
        model_visible: true,
        fallback: null,
      })),
    };
  }
  return invoke<FolderSet>("approve_structure", { request: { folders: proposal.folders } });
}
