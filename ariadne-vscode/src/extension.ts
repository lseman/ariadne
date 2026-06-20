import * as vscode from 'vscode';
import { AriadneClient } from './mcp';
import { registerCommands } from './commands';
import { registerSidebar } from './sidebar';

let client: AriadneClient;

export function activate(context: vscode.ExtensionContext) {
  const config = vscode.workspace.getConfiguration('ariadne');
  client = new AriadneClient({
    binaryPath: config.get('binaryPath', ''),
    dbPath: config.get('dbPath', ''),
  });

  // Check if Ariadne is available on activation
  checkAvailability(client);

  // Register commands
  registerCommands(context, client);

  // Register sidebar tree view
  registerSidebar(context, client);

  // Re-check availability on configuration change
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('ariadne')) {
        checkAvailability(client);
      }
    })
  );

  // Dispose on deactivate
  context.subscriptions.push({ dispose: () => client?.dispose() });
}

async function checkAvailability(client: AriadneClient) {
  const available = await client.checkAvailable();
  if (!available) {
    vscode.window.showWarningMessage(
      'Ariadne binary not found. Run `ariadne --help` in terminal or set "ariadne.binaryPath" in settings.'
    );
  }
}

export function deactivate() {
  client?.dispose();
}
