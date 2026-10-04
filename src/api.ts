import { invoke } from "@tauri-apps/api/core";
import type {
  ChartMode,
  ChildPage,
  ComparisonSummary,
  JobSummary,
  Metric,
  NodeDetails,
  RootPage,
  SnapshotSide,
  ExtensionPage,
  FullTreemapData,
  TreemapHit,
  TreemapRect,
  FileCategoriesData,
} from "./types";
export const api = {
  startComparison: (beforePath: string, afterPath: string) =>
    invoke<string>("start_comparison", { beforePath, afterPath }),
  cancelComparison: (jobId: string) =>
    invoke<void>("cancel_comparison", { jobId }),
  getJob: (jobId: string) => invoke<JobSummary>("get_job", { jobId }),
  getComparison: () => invoke<ComparisonSummary | null>("get_comparison"),
  listRoots: (
    comparisonId: string,
    side: SnapshotSide,
    cursor: string | null = null,
  ) => invoke<RootPage>("list_roots", { comparisonId, side, cursor }),
  listChildren: (
    comparisonId: string,
    parentId: string | null,
    changesOnly: boolean,
    cursor: string | null = null,
  ) =>
    invoke<ChildPage>("list_children", {
      comparisonId,
      parentId,
      changesOnly,
      cursor,
    }),
  getDetails: (comparisonId: string, nodeId: string) =>
    invoke<NodeDetails>("get_details", { comparisonId, nodeId }),
  getFileCategories: (comparisonId: string) =>
    invoke<FileCategoriesData>("get_file_categories", { comparisonId }),
  listExtensions: (
    comparisonId: string,
    parentId: string | null,
    side: SnapshotSide,
    metric: Metric,
    cursor: string | null = null,
  ) =>
    invoke<ExtensionPage>("list_extensions", {
      comparisonId,
      parentId,
      side,
      metric,
      cursor,
    }),
  getFullTreemap: (
    comparisonId: string,
    metric: Metric,
    mode: ChartMode,
    maxDepth: number,
  ) =>
    invoke<FullTreemapData>("get_full_treemap", {
      comparisonId,
      metric,
      mode,
      maxDepth,
    }),
  hitTestTreemap: (
    comparisonId: string,
    metric: Metric,
    mode: ChartMode,
    x: number,
    y: number,
    maxDepth: number,
  ) =>
    invoke<TreemapHit | null>("hit_test_treemap", {
      comparisonId,
      metric,
      mode,
      x,
      y,
      maxDepth,
    }),
  getTreemapBounds: (
    comparisonId: string,
    metric: Metric,
    mode: ChartMode,
    nodeId: string,
    maxDepth: number,
  ) =>
    invoke<TreemapRect | null>("get_treemap_bounds", {
      comparisonId,
      metric,
      mode,
      nodeId,
      maxDepth,
    }),
};
