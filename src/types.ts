export type Status =
  | "added"
  | "removed"
  | "modified"
  | "unchanged"
  | "typeChanged";
export type Metric = "size" | "allocated";
export type ChartMode = "before" | "after" | "delta";
export interface ApiError {
  code: string;
  message: string;
  source?: string;
  record?: number;
  column?: string;
}
export interface Entry {
  path: string;
  kind: "file" | "directory";
  size: string;
  allocated: string;
  modified: string | null;
  attributes: string | null;
  files: string | null;
  folders: string | null;
  mft: string | null;
  parentMft: string | null;
  accessed: string | null;
  created: string | null;
  directSize: string | null;
  directAllocated: string | null;
  driveCapacity: string | null;
  freeSpace: string | null;
  usedSpace: string | null;
  reservedSpace: string | null;
  hardlinkCount: number;
}
export interface SideValue {
  kind: "file" | "directory";
  size: string;
  allocated: string;
  modified: string | null;
  attributes: string | null;
}
export interface ChildRow {
  nodeId: string;
  name: string;
  expandable: boolean;
  status: Status;
  hasChanges: boolean;
  changedDescendantCount: number;
  childCount: number;
  before: SideValue | null;
  after: SideValue | null;
  sizeDelta: string;
  allocatedDelta: string;
}
export interface ChildPage {
  rows: ChildRow[];
  nextCursor: string | null;
  totalChildren: number;
}
export interface NodeDetails {
  nodeId: string;
  parentId: string | null;
  before: Entry | null;
  after: Entry | null;
  sizeDelta: string;
  allocatedDelta: string;
}
export type SnapshotSide = "before" | "after";
export interface Root {
  nodeId: string;
  path: string;
  kind: "file" | "directory";
  size: string;
  allocated: string;
}
export interface RootPage {
  rows: Root[];
  nextCursor: string | null;
  totalRoots: number;
}
export interface SourceSummary {
  path: string;
  description: string | null;
  rows: number;
  files: number;
  folders: number;
  roots: Root[];
  rootCount: number;
  rootsTruncated: boolean;
  size: string;
  allocated: string;
}
export interface ComparisonSummary {
  comparisonId: string;
  before: SourceSummary;
  after: SourceSummary;
  statuses: Record<"files" | "folders", Record<Status, number>>;
  warnings: string[];
}
export interface JobSummary {
  jobId: string;
  state: "running" | "ready" | "cancelled" | "failed";
  phase: "before" | "after" | "indexing" | "comparing";
  bytesRead: string;
  totalBytes: string;
  rows: number;
  comparisonId?: string;
  error?: ApiError;
}
export interface TypeValue {
  size: string;
  allocated: string;
  files: number;
}
export interface ExtensionItem {
  extension: string;
  size: string;
  allocated: string;
  files: number;
}
export interface ExtensionPage {
  rows: ExtensionItem[];
  nextCursor: string | null;
  totalExtensions: number;
  total: TypeValue;
  warnings: string[];
}
export interface TreemapRect {
  x: number;
  y: number;
  width: number;
  height: number;
}
export interface TreemapLabel extends TreemapRect {
  nodeId: string;
  name: string;
  path: string;
  kind: "file" | "directory";
  weight: string;
}
export interface FullTreemapData {
  imageDataUrl: string;
  atlasWidth: number;
  atlasHeight: number;
  fileCount: number;
  visibleFileCount: number;
  weightTotal: string;
  positiveTotal: string;
  negativeTotal: string;
  netDelta: string;
  exportedTotal: string;
  labels: TreemapLabel[];
  warnings: string[];
  maxDepth: number;
  renderedBlockCount: number;
  addedFileCount: number;
}
export interface TreemapHit {
  nodeId: string;
  name: string;
  path: string;
  extension: string;
  weight: string;
  value: string;
  status: Status;
  kind: "file" | "directory";
  collapsed: boolean;
  rect: TreemapRect;
}
export interface FileCategoryItem {
  category: string;
  before: TypeValue;
  after: TypeValue;
}
export interface FileCategoriesData {
  items: FileCategoryItem[];
  before: TypeValue;
  after: TypeValue;
  warnings: string[];
}
