# Ariadne - Code Graph Explorer (VS Code Extension)

Explore codebases using Ariadne's code graph from within VS Code.

## Features

- **Sidebar tree view** with all Ariadne operations
- **Command palette** integration for all operations
- **Result panels** with formatted output
- **Configurable** binary path and database location

## Operations

| Operation | Description |
|-----------|-------------|
| Search | Search the code graph by query |
| Code Context | Get bounded context for a symbol/file |
| Impact Analysis | See what changes would affect |
| Traverse Dependencies | Walk dependencies of a symbol |
| Find Paths | Find paths between two symbols |
| Architecture Overview | High-level module structure |
| Bridge Nodes | Find critical bridge nodes |
| Gaps | Find gaps in the graph |
| Core Nodes | Show most central nodes |
| Cycles | Find dependency cycles |
| Diagnostics | Check graph health |

## Installation

1. Install the Ariadne CLI: https://github.com/seman/ariadne
2. Open this extension directory in VS Code
3. Run `npm install`
4. Press F5 to launch the extension host
5. To publish: `vsce package`

## Configuration

| Setting | Description | Default |
|---------|-------------|---------|
| `ariadne.binaryPath` | Path to the Ariadne binary | (auto-detect from PATH) |
| `ariadne.dbPath` | Path to the Ariadne SQLite database | (auto-detect) |

## Usage

1. Open the Ariadne sidebar (click the graph icon in activity bar)
2. Click any operation to run it
3. Or use the command palette: `Ctrl+Shift+P` → "Ariadne: <operation>"
4. For symbol-aware operations, select a symbol or place cursor before running
