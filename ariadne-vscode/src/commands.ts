import * as vscode from 'vscode';
import { AriadneClient } from './mcp';
import { AriadneOperation } from './types';
import { AriadneResultPanel } from './resultPanel';

export function registerCommands(context: vscode.ExtensionContext, client: AriadneClient) {
  const extensionUri = context.extensionUri;
  const cmds = [
    { name: 'search', title: 'Ariadne: Search Graph', icon: '$(search)' },
    { name: 'minimal_context', title: 'Ariadne: Get Code Context', icon: '$(file-code)' },
    { name: 'impact', title: 'Ariadne: Impact Analysis', icon: '$(graph)' },
    { name: 'traverse', title: 'Ariadne: Traverse Dependencies', icon: '$(project)' },
    { name: 'paths', title: 'Ariadne: Find Paths', icon: '$(link)' },
    { name: 'architecture', title: 'Ariadne: Architecture Overview', icon: '$(list-tree)' },
    { name: 'bridge_nodes', title: 'Ariadne: Find Bridge Nodes', icon: '$(git-merge)' },
    { name: 'gaps', title: 'Ariadne: Find Gaps', icon: '$(warning)' },
    { name: 'core', title: 'Ariadne: Core Nodes', icon: '$(star-empty)' },
    { name: 'cycles', title: 'Ariadne: Find Cycles', icon: '$(refresh)' },
    { name: 'diagnostics', title: 'Ariadne: Diagnostics', icon: '$(output)' },
  ];

  for (const { name } of cmds) {
    context.subscriptions.push(
      vscode.commands.registerCommand(`ariadne.${name}`, async () => {
        await runOperation(extensionUri, client, name as AriadneOperation);
      })
    );
  }

  context.subscriptions.push(
    vscode.commands.registerCommand('ariadne.configure', async () => {
      await vscode.window.showInputBox({
        prompt: 'Ariadne binary path (leave empty for PATH)',
        value: vscode.workspace.getConfiguration('ariadne').get('binaryPath') || '',
      }).then((value) =>
        vscode.workspace.getConfiguration('ariadne').update('binaryPath', value || '', vscode.ConfigurationTarget.Global)
      );
      await vscode.window.showInputBox({
        prompt: 'Ariadne database path (empty to auto-detect)',
        value: vscode.workspace.getConfiguration('ariadne').get('dbPath') || '',
      }).then((value) =>
        vscode.workspace.getConfiguration('ariadne').update('dbPath', value || '', vscode.ConfigurationTarget.Global)
      );
    })
  );
}

async function runOperation(extensionUri: vscode.Uri, client: AriadneClient, operation: AriadneOperation) {
  // Check availability
  const available = await client.checkAvailable();
  if (!available) {
    vscode.window.showErrorMessage('Ariadne binary not found. Install it or configure the path in settings.');
    return;
  }

  // Get target from active editor or prompt user
  const editor = vscode.window.activeTextEditor;
  let target = '';
  let extraParams: Record<string, string> = {};

  if (editor) {
    const selection = editor.selection;
    const symbolAtCursor = editor.document.getText(selection) || extractSymbolAtPosition(editor.document, editor.selection.active);

    if (symbolAtCursor && !selection.isEmpty) {
      target = symbolAtCursor;
    } else {
      target = editor.document.fileName;
    }

    if (['minimal_context', 'impact', 'traverse'].includes(operation)) {
      const useSymbol = await vscode.window.showQuickPick(
        [
          { label: 'Use selection/cursor symbol', description: symbolAtCursor || '(none)' },
          { label: 'Use current file', description: editor.document.fileName },
          { label: 'Custom input', description: 'Type your own query' },
        ],
        { placeHolder: 'Select target for Ariadne operation' }
      );
      if (!useSymbol) return;
      if (useSymbol.label === 'Custom input') {
        target = await vscode.window.showInputBox({ prompt: `Enter target for "${operation}"` }) || '';
        if (!target) return;
      } else {
        target = useSymbol.label;
      }
    }

    if (operation === 'paths') {
      const source = await vscode.window.showInputBox({ prompt: 'Source symbol or file' });
      const dest = await vscode.window.showInputBox({ prompt: 'Target symbol or file' });
      if (!source || !dest) return;
      extraParams = { source, target: dest };
    }
  } else {
    target = await vscode.window.showInputBox({ prompt: `Enter target for "${operation}"` }) || '';
    if (!target) return;
  }

  if (['minimal_context', 'impact', 'traverse'].includes(operation)) {
    extraParams.target = target;
  }

  if (operation === 'search') {
    const query = target;
    extraParams = { query };
  }

  // Show loading
  await vscode.window.withProgress(
    { location: vscode.ProgressLocation.Notification, title: `Ariadne: ${operation}...`, cancellable: false },
    async () => {
      try {
        const result = await client.callOperation(operation, extraParams);
        AriadneResultPanel.createOrShow(extensionUri, operation, result);
      } catch (err) {
        vscode.window.showErrorMessage(`Ariadne error: ${err instanceof Error ? err.message : String(err)}`);
      }
    }
  );
}

function extractSymbolAtPosition(document: vscode.TextDocument, position: vscode.Position): string {
  const line = document.lineAt(position.line).text;
  const regex = /[\w.$]+/g;
  let match: RegExpExecArray | null;
  while ((match = regex.exec(line)) !== null) {
    const start = match.index;
    const end = match.index + match[0].length;
    if (position.character >= start && position.character <= end) {
      return match[0];
    }
  }
  return '';
}
