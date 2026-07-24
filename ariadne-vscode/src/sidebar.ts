import * as vscode from 'vscode';
import { AriadneClient } from './mcp';
import { AriadneOperation } from './types';

interface AriadneTreeNode extends vscode.TreeItem {
  command?: vscode.Command;
  children?: AriadneTreeNode[];
}

export class AriadneTreeProvider implements vscode.TreeDataProvider<AriadneTreeNode> {
  private _onDidChangeTreeData = new vscode.EventEmitter<AriadneTreeNode | undefined>();
  readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

  constructor(private client: AriadneClient) {}

  getTreeItem(element: AriadneTreeNode): vscode.TreeItem {
    return element;
  }

  getChildren(element?: AriadneTreeNode): AriadneTreeNode[] {
    if (element) {
      return element.children || [];
    }

    const operations: { op: AriadneOperation; label: string; icon: string }[] = [
      { op: 'search', label: 'Search', icon: 'search' },
      { op: 'minimal_context', label: 'Code Context', icon: 'file-code' },
      { op: 'impact', label: 'Impact Analysis', icon: 'graph' },
      { op: 'traverse', label: 'Traverse Dependencies', icon: 'project' },
      { op: 'paths', label: 'Find Paths', icon: 'link' },
      { op: 'architecture', label: 'Architecture Overview', icon: 'list-tree' },
      { op: 'bridge_nodes', label: 'Bridge Nodes', icon: 'git-merge' },
      { op: 'gaps', label: 'Gaps', icon: 'warning' },
      { op: 'core', label: 'Core Nodes', icon: 'star-empty' },
      { op: 'cycles', label: 'Cycles', icon: 'refresh' },
    ];

    return operations.map((op) => ({
      label: op.label,
      id: op.op,
      iconPath: new vscode.ThemeIcon(op.icon as any),
      command: {
        command: `ariadne.${op.op}`,
        title: `Run ${op.label}`,
      },
      tooltip: `Run "${op.op}" operation`,
    }));
  }

  getParent?(_element: AriadneTreeNode): vscode.TreeItem | null {
    return null;
  }
}

export function registerSidebar(context: vscode.ExtensionContext, client: AriadneClient) {
  const provider = new AriadneTreeProvider(client);
  const view = vscode.window.createTreeView('ariadne.operations', {
    treeDataProvider: provider,
    showCollapseAll: false,
  });
  context.subscriptions.push(view);
}
