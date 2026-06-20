export interface AriadneConfig {
  binaryPath: string;
  dbPath: string;
}

export interface AriadneMcpMessage {
  jsonrpc: string;
  id?: number | string;
  result?: { content: { type: string; text: string }[] };
  error?: { code: number; message: string };
}

export interface AriadneSearchResult {
  id?: string;
  label: string;
  file?: string;
  line?: number;
  score?: number;
  qualified_name?: string;
}

export interface AriadneEdge {
  id: string;
  label: string;
  source?: string;
  target?: string;
}

export interface AriadneContextResult {
  node?: string;
  edges?: AriadneEdge[];
  code?: string;
  [key: string]: unknown;
}

export interface AriadneImpactResult {
  affected?: string[];
  changes?: string;
  [key: string]: unknown;
}

export interface AriadneTraverseResult {
  nodes?: { id: string; label: string; file?: string }[];
  edges?: AriadneEdge[];
  [key: string]: unknown;
}

export interface AriadnePathsResult {
  paths?: string[][];
  [key: string]: unknown;
}

export interface AriadneArchitectureResult {
  modules?: { name: string; nodes: number; edges: number }[];
  [key: string]: unknown;
}

export interface AriadneDiagnosticsResult {
  call_resolution_rate?: number;
  index_coverage?: number;
  [key: string]: unknown;
}

export interface AriadneBridgeNodeResult {
  bridge_nodes?: { id: string; label: string; score: number }[];
  [key: string]: unknown;
}

export interface AriadneGapResult {
  gaps?: { id: string; label: string; description?: string }[];
  [key: string]: unknown;
}

export interface AriadneCoreNodeResult {
  core_nodes?: { id: string; label: string; centrality: number }[];
  [key: string]: unknown;
}

export interface AriadneCycleResult {
  cycles?: string[][];
  [key: string]: unknown;
}

export type AriadneOperation =
  | 'search'
  | 'minimal_context'
  | 'impact'
  | 'traverse'
  | 'paths'
  | 'architecture_overview'
  | 'architecture'
  | 'cycles'
  | 'core'
  | 'bridge_nodes'
  | 'gaps'
  | 'diagnostics'
  | 'flows'
  | 'affected_flows'
  | 'detect_changes'
  | 'review_context';
