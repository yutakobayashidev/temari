import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { ClassificationBasis, FolderSet, PlanPreview, Proposal, ScanPreview } from "./types";

const demoProposal: Proposal = {
  version: 2,
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

const demoMoves: Array<[string, string, string, ClassificationBasis]> = [
  ["project-brief.pdf", "d0001", "Documents/Work/project-brief.pdf", "name"],
  ["meeting-notes.md", "d0001", "Documents/Work/meeting-notes.md", "name"],
  ["roadmap-q3.pdf", "d0001", "Documents/Work/roadmap-q3.pdf", "content"],
  ["research-links.md", "d0001", "Documents/Work/research-links.md", "name"],
  ["receipt-june.pdf", "d0002", "Documents/Personal/receipt-june.pdf", "name"],
  ["tax-form.pdf", "d0002", "Documents/Personal/tax-form.pdf", "content"],
  ["travel-booking.pdf", "d0002", "Documents/Personal/travel-booking.pdf", "name"],
  ["screenshot-001.png", "d0003", "Media/Images/screenshot-001.png", "name"],
  ["screenshot-002.png", "d0003", "Media/Images/screenshot-002.png", "name"],
  ["portrait.png", "d0003", "Media/Images/portrait.png", "content"],
  ["diagram-export.png", "d0003", "Media/Images/diagram-export.png", "name"],
  ["voice-note-01.mp3", "d0004", "Media/Audio/voice-note-01.mp3", "name"],
  ["interview.mp3", "d0004", "Media/Audio/interview.mp3", "content"],
  ["ambient-loop.wav", "d0004", "Media/Audio/ambient-loop.wav", "extension_fallback"],
  ["source-bundle.zip", "d0005", "Packages/source-bundle.zip", "name"],
  ["temari-linux.tar.gz", "d0005", "Packages/temari-linux.tar.gz", "name"],
  ["utilities.zip", "d0005", "Packages/utilities.zip", "extension_fallback"],
  ["installer.dmg", "d0005", "Packages/installer.dmg", "extension_fallback"],
];

const demoFolders: FolderSet["folders"] = demoProposal.folders.map((folder, index) => ({
  ...folder,
  id: `d${String(index + 1).padStart(4, "0")}`,
  model_visible: true,
  fallback: null,
}));

const demoPlan: PlanPreview = {
  sha256: "6a122287a651db254964911b346f5c28ecf20434729971d9ea601c83c36630c8",
  plan: {
    version: 4,
    source: demoProposal.source,
    source_identity: { device: 1, inode: 42 },
    scope: demoProposal.scope,
    collision_policy: "rename",
    folders: demoFolders,
    directories: [
      "Documents",
      "Media",
      "Packages",
      "Documents/Personal",
      "Documents/Work",
      "Media/Audio",
      "Media/Images",
    ],
    entries: demoMoves.map(([sourcePath, destinationId, destinationPath, basis], index) => ({
      file_id: `f${String(index + 1).padStart(6, "0")}`,
      source_path: sourcePath,
      source_fingerprint: { identity: { device: 1, inode: 100 + index }, size: 1024 + index, sha256: "0".repeat(64) },
      destination_id: destinationId,
      requested_destination: destinationPath,
      destination_path: destinationPath,
      reasoning: null,
      classification_basis: basis,
      rule_id: null,
    })),
  },
};

let demoApproved: FolderSet | null = null;

function isTauri(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

export type ConfigLocation = {
  path: string | null;
  defaultPath: string;
};

export async function defaultConfigLocation(): Promise<ConfigLocation> {
  if (!isTauri()) {
    return {
      path: "/Users/you/Library/Application Support/dev.yutakobayashidev.temari/config.toml",
      defaultPath: "/Users/you/Library/Application Support/dev.yutakobayashidev.temari/config.toml",
    };
  }
  return invoke<ConfigLocation>("default_config_location");
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
  if (!isTauri()) return { ...structuredClone(demoProposal), source };
  return invoke<Proposal>("propose_structure", {
    request: { configPath, source, recursiveRoots: [], maxFolders: 8 },
  });
}

export async function approveStructure(proposal: Proposal): Promise<FolderSet> {
  if (!isTauri()) {
    demoApproved = {
      version: 3,
      source: proposal.source,
      scope: proposal.scope,
      folders: proposal.folders.map((folder, index) => ({
        ...folder,
        id: `d${String(index + 1).padStart(4, "0")}`,
        model_visible: true,
        fallback: null,
      })),
    };
    return structuredClone(demoApproved);
  }
  return invoke<FolderSet>("approve_structure", { request: { folders: proposal.folders } });
}

export async function previewPlan(): Promise<PlanPreview> {
  if (!isTauri()) {
    const preview = structuredClone(demoPlan);
    if (!demoApproved) return preview;
    preview.plan.source = demoApproved.source;
    preview.plan.folders = structuredClone(demoApproved.folders);
    const foldersById = new Map(demoApproved.folders.map((folder) => [folder.id, folder.path]));
    for (const entry of preview.plan.entries) {
      const folder = foldersById.get(entry.destination_id);
      if (!folder) continue;
      const fileName = entry.source_path.split("/").at(-1) ?? entry.source_path;
      entry.destination_path = `${folder}/${fileName}`;
      entry.requested_destination = entry.destination_path;
    }
    const directories = new Set<string>();
    for (const folder of demoApproved.folders) {
      const parts = folder.path.split("/");
      for (let index = 1; index <= parts.length; index += 1) directories.add(parts.slice(0, index).join("/"));
    }
    preview.plan.directories = [...directories].sort((left, right) => {
      const depth = left.split("/").length - right.split("/").length;
      return depth || left.localeCompare(right);
    });
    return preview;
  }
  return invoke<PlanPreview>("preview_plan");
}
