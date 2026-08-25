// Interactive Ask / Tell REPL rendered as a VSCode Pseudoterminal.
//
// Each line the user enters is a single KIF sentence.  The first
// token decides the mode:
//
//   ask (query …)     — proof search (`kernel.ask`)
//   tell (axiom …)    — session assertion (`kernel.tell`)
//   :help / :quit     — meta-commands
//   (bare form)       — treated as `ask`
//
// The REPL owns a single `session` string that scopes every tell.
// A new terminal gets a fresh session name so two parallel REPLs
// don't see each other's assertions.
//
// Minimal line editing: backspace, Enter, up/down history.  This
// intentionally stays dumb -- a full line editor is out of scope
// and users who want one can pipe into `sumo serve` directly.

import {
    EventEmitter,
    Pseudoterminal,
    TerminalDimensions,
    window,
} from 'vscode';

import { AskResult, SumoKernelClient, TellResult } from './kernelClient';

const PROMPT = 'sumo> ';
const SESSION_PREFIX = 'repl-';

export function openReplCommand(kernel: SumoKernelClient): void {
    const session = SESSION_PREFIX + Math.random().toString(36).slice(2, 10);
    const pty = new SumoRepl(kernel, session);
    const terminal = window.createTerminal({
        name:  'SUMO REPL',
        pty,
    });
    terminal.show(false);
}

class SumoRepl implements Pseudoterminal {
    private readonly writeEmitter = new EventEmitter<string>();
    readonly onDidWrite = this.writeEmitter.event;

    private readonly closeEmitter = new EventEmitter<void | number>();
    readonly onDidClose = this.closeEmitter.event;

    private buffer = '';
    private cursor = 0;
    private history: string[] = [];
    private historyIdx = 0;
    private busy = false;

    constructor(
        private readonly kernel:  SumoKernelClient,
        private readonly session: string,
    ) {}

    open(_initialDimensions: TerminalDimensions | undefined): void {
        this.writeEmitter.fire(
            `SUMO Ask/Tell REPL — session "${this.session}"\r\n` +
            `Type a KIF sentence to ask, or prefix with "tell" to assert.\r\n` +
            `Commands: :help  :quit\r\n\r\n`,
        );
        this.prompt();
    }

    close(): void {
        // Terminal is gone; nothing to tear down on the client
        // side.  The kernel itself outlives the REPL session.
    }

    async handleInput(data: string): Promise<void> {
        if (this.busy) {
            // Swallow keystrokes while a request is in flight so a
            // fast user can't interleave a second line before the
            // first comes back.  They're free to Ctrl+C via the
            // terminal close button if they want to bail.
            return;
        }

        for (let i = 0; i < data.length; i++) {
            const ch = data.charAt(i);
            // Escape sequences for arrow keys: ESC [ A | B | C | D.
            if (ch === '\x1b' && data[i + 1] === '[') {
                const arrow = data[i + 2];
                i += 2;
                if      (arrow === 'A') { this.historyPrev(); }
                else if (arrow === 'B') { this.historyNext(); }
                // Left / right arrows: ignored (no cursor movement in MVP).
                continue;
            }
            if (ch === '\r' || ch === '\n') {
                this.writeEmitter.fire('\r\n');
                const line = this.buffer;
                this.buffer = '';
                this.cursor = 0;
                await this.runLine(line);
                if (!this.busy) { this.prompt(); }
                continue;
            }
            // Ctrl-C: abandon the current line.
            if (ch === '\x03') {
                this.writeEmitter.fire('^C\r\n');
                this.buffer = '';
                this.cursor = 0;
                this.prompt();
                continue;
            }
            // Backspace / DEL.
            if (ch === '\x7f' || ch === '\b') {
                if (this.buffer.length > 0) {
                    this.buffer = this.buffer.slice(0, -1);
                    this.cursor--;
                    this.writeEmitter.fire('\b \b');
                }
                continue;
            }
            if (ch === '\x1b') {
                // Bare ESC: drop.
                continue;
            }
            this.buffer += ch;
            this.cursor++;
            this.writeEmitter.fire(ch);
        }
    }

    private prompt(): void {
        this.writeEmitter.fire(PROMPT);
    }

    private historyPrev(): void {
        if (this.history.length === 0) { return; }
        this.historyIdx = Math.max(0, this.historyIdx - 1);
        this.replaceBuffer(this.history[this.historyIdx] ?? '');
    }

    private historyNext(): void {
        if (this.history.length === 0) { return; }
        this.historyIdx = Math.min(this.history.length, this.historyIdx + 1);
        const next = this.historyIdx === this.history.length
            ? '' : this.history[this.historyIdx];
        this.replaceBuffer(next ?? '');
    }

    private replaceBuffer(next: string): void {
        while (this.buffer.length > 0) {
            this.buffer = this.buffer.slice(0, -1);
            this.writeEmitter.fire('\b \b');
        }
        this.buffer = next;
        this.cursor = next.length;
        this.writeEmitter.fire(next);
    }

    private async runLine(line: string): Promise<void> {
        const trimmed = line.trim();
        if (trimmed.length === 0) { return; }

        this.history.push(trimmed);
        this.historyIdx = this.history.length;

        if (trimmed === ':quit' || trimmed === ':exit') {
            this.writeEmitter.fire('bye\r\n');
            this.closeEmitter.fire();
            return;
        }
        if (trimmed === ':help') {
            this.writeEmitter.fire(
                `Commands:\r\n` +
                `  tell (…)    — add an axiom to the session KB\r\n` +
                `  ask  (…)    — ask a conjecture against the KB\r\n` +
                `  (…)         — same as ask\r\n` +
                `  :help       — this message\r\n` +
                `  :quit       — close the REPL\r\n`,
            );
            return;
        }

        // Strip an optional leading "ask " / "tell " directive.
        let mode: 'ask' | 'tell' = 'ask';
        let body = trimmed;
        const m = /^(ask|tell)\s+/i.exec(trimmed);
        if (m) {
            mode = m[1].toLowerCase() as 'ask' | 'tell';
            body = trimmed.slice(m[0].length);
        }

        this.busy = true;
        try {
            if (mode === 'tell') {
                const res = await this.kernel.tell(body, this.session);
                this.renderTell(res);
            } else {
                const res = await this.kernel.ask(body, { session: this.session });
                this.renderAsk(res);
            }
        } catch (err) {
            this.writeEmitter.fire(`error: ${String(err)}\r\n`);
        } finally {
            this.busy = false;
            this.prompt();
        }
    }

    private renderTell(r: TellResult): void {
        this.writeEmitter.fire(r.ok ? 'ok\r\n' : 'not ok\r\n');
        for (const w of r.warnings) { this.writeEmitter.fire(`  warning: ${w}\r\n`); }
        for (const e of r.errors)   { this.writeEmitter.fire(`  error:   ${e}\r\n`); }
    }

    private renderAsk(r: AskResult): void {
        this.writeEmitter.fire(`${r.status}\r\n`);
        if (r.bindings.length > 0) {
            this.writeEmitter.fire('bindings:\r\n');
            for (const b of r.bindings) { this.writeEmitter.fire(`  ${b}\r\n`); }
        }
        if (r.proofKif.length > 0) {
            this.writeEmitter.fire(`proof (${r.proofKif.length} step${r.proofKif.length === 1 ? '' : 's'}):\r\n`);
            for (const step of r.proofKif) {
                // Indent each step; proofs can span lines.
                const lines = step.split(/\r?\n/);
                for (const l of lines) { this.writeEmitter.fire(`  ${l}\r\n`); }
            }
        }
    }
}
