import * as vscode from 'vscode';

export class AriadneResultPanel {
  public static currentPanel: AriadneResultPanel | undefined;
  private readonly panel: vscode.WebviewPanel;
  private readonly extensionUri: vscode.Uri;
  private readonly operation: string;

  constructor(extensionUri: vscode.Uri, operation: string, result: string) {
    this.extensionUri = extensionUri;
    this.operation = operation;
    this.panel = vscode.window.createWebviewPanel(
      'ariadneResult',
      `Ariadne: ${operation}`,
      vscode.ViewColumn.Two,
      {
        enableScripts: false,
        retainContextWhenHidden: true,
      }
    );

    this.panel.onDidDispose(() => {
      AriadneResultPanel.currentPanel = undefined;
    }, null);

    this.update(result);
  }

  update(result: string) {
    if (!this.panel) return;
    const formatted = this.formatResult(result);
    this.panel.webview.html = this.getHtml(formatted);
  }

  private formatResult(result: string): string {
    // Try to parse as JSON for pretty display
    let parsed: unknown;
    try {
      parsed = JSON.parse(result);
    } catch {
      // Not JSON, return as plain text
      return `<pre>${escapeHtml(result)}</pre>`;
    }

    // Pretty-print JSON
    const jsonStr = JSON.stringify(parsed, null, 2);
    return `<pre style="white-space: pre-wrap; word-wrap: break-word; font-family: var(--vscode-editor-font-family); font-size: var(--vscode-editor-font-size); color: var(--vscode-editor-foreground);">${escapeHtml(jsonStr)}</pre>`;
  }

  private getHtml(content: string): string {
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none';">
  <title>Ariadne Result</title>
  <style>
    body {
      padding: 16px;
      margin: 0;
      background-color: var(--vscode-editor-background);
      color: var(--vscode-editor-foreground);
    }
    pre {
      white-space: pre-wrap;
      word-wrap: break-word;
      font-family: var(--vscode-editor-font-family);
      font-size: var(--vscode-editor-font-size);
      line-height: 1.5;
    }
    .operation-badge {
      display: inline-block;
      padding: 2px 8px;
      border-radius: 4px;
      background: var(--vscode-badge-background);
      color: var(--vscode-badge-foreground);
      font-size: 12px;
      margin-bottom: 8px;
      text-transform: uppercase;
      font-weight: bold;
    }
    .copy-btn {
      position: fixed;
      top: 16px;
      right: 16px;
      padding: 4px 12px;
      background: var(--vscode-button-background);
      color: var(--vscode-button-foreground);
      border: none;
      border-radius: 4px;
      cursor: pointer;
    }
  </style>
</head>
<body>
  <span class="operation-badge">${escapeHtml(this.operation)}</span>
  <button class="copy-btn" onclick="navigator.clipboard.writeText(document.body.innerText)">Copy</button>
  ${content}
</body>
</html>`;
  }

  static createOrShow(extensionUri: vscode.Uri, operation: string, result: string) {
    const panel = AriadneResultPanel.currentPanel;
    if (panel) {
      panel.update(result);
      return panel;
    }
    AriadneResultPanel.currentPanel = new AriadneResultPanel(extensionUri, operation, result);
    return AriadneResultPanel.currentPanel;
  }
}

function escapeHtml(str: string): string {
  return str
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#039;');
}
