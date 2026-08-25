// Explorer tree view for this window's knowledge base.
//
// Per-window-per-KB model: the tree has exactly one root (the
// active KB) when a session is open, or a single "No KB active"
// placeholder otherwise.  Children are the constituent files,
// clickable to open.  Context-menu actions (add / remove file,
// close KB, open other KB in new window) are contributed via
// package.json `view/item/context` menu entries.

import * as path from 'path';
import {
    Command,
    EventEmitter,
    ThemeIcon,
    TreeDataProvider,
    TreeItem,
    TreeItemCollapsibleState,
    Uri,
    Event,
} from 'vscode';

import { ActiveKb, KbState } from './kbSession';

// -- Node shapes -------------------------------------------------------------

type Node = KbNode | FileNode | EmptyNode;

interface KbNode    { kind: 'kb';    active: ActiveKb }
interface FileNode  { kind: 'file';  active: ActiveKb; file: string }
interface EmptyNode { kind: 'empty' }

// -- Provider ----------------------------------------------------------------

export class KbTreeProvider implements TreeDataProvider<Node> {
    private emitter = new EventEmitter<Node | undefined>();
    readonly onDidChangeTreeData: Event<Node | undefined> = this.emitter.event;

    constructor(private readonly state: KbState) {}

    refresh(): void { this.emitter.fire(undefined); }

    getTreeItem(element: Node): TreeItem {
        switch (element.kind) {
            case 'kb':    return kbTreeItem(element.active);
            case 'file':  return fileTreeItem(element);
            case 'empty': return emptyPlaceholder();
        }
    }

    getChildren(element?: Node): Node[] {
        if (!element) {
            const active = this.state.get();
            if (!active) { return [{ kind: 'empty' }]; }
            return [{ kind: 'kb', active }];
        }
        if (element.kind === 'kb') {
            const files = Array.from(element.active.files).sort();
            return files.map(file => ({ kind: 'file', active: element.active, file }));
        }
        return [];
    }
}

// -- Node -> TreeItem --------------------------------------------------------

function kbTreeItem(active: ActiveKb): TreeItem {
    const item = new TreeItem(active.name, TreeItemCollapsibleState.Expanded);
    const n    = active.files.size;
    item.description = `${n} file${n === 1 ? '' : 's'}`;
    item.tooltip     =
        `${active.source === 'config' ? 'Config-declared' : 'Temporary'} knowledge base\n` +
        `${n} constituent${n === 1 ? '' : 's'}`;
    item.iconPath    = new ThemeIcon(
        active.source === 'config' ? 'library' : 'file-submodule',
    );
    item.contextValue = `sumoKb.${active.source}`;
    item.id = `sumo-kb::root`;
    return item;
}

function fileTreeItem(node: FileNode): TreeItem {
    const item = new TreeItem(
        path.basename(node.file),
        TreeItemCollapsibleState.None,
    );
    item.description = path.relative(
        process.env.HOME ?? '/',
        path.dirname(node.file),
    );
    item.tooltip     = node.file;
    item.resourceUri = Uri.file(node.file);
    item.iconPath    = ThemeIcon.File;
    item.contextValue = `sumoKb.file.${node.active.source}`;
    item.id = `sumo-kb::file::${node.file}`;
    item.command = <Command>{
        command:   'vscode.open',
        title:     'Open File',
        arguments: [Uri.file(node.file)],
    };
    return item;
}

function emptyPlaceholder(): TreeItem {
    const item = new TreeItem(
        'No KB active',
        TreeItemCollapsibleState.None,
    );
    item.description = 'Run "SUMO: Load Knowledge Base…" or open a .kif file';
    item.iconPath = new ThemeIcon('circle-outline');
    item.contextValue = 'sumoKb.empty';
    item.id = 'sumo-kb::empty';
    return item;
}
