import { GitOrigin, Origin } from "./Origin";
import { MIDLEVEL, MERGE } from "../constants";

/** One loaded KIF file: its worker-session name, where it came from, and the
 *  text last ingested for it. */
export class Constituent {
    name: string;
    origin: Origin;
    text?: string;

    constructor(name: string, origin: Origin, text?: string) {
        this.name = name;
        this.origin = origin;
        this.text = text;
    }

    static defaults(): Constituent[] {
        return [
            new Constituent(MERGE, GitOrigin.default()),
            new Constituent(MIDLEVEL, GitOrigin.default())
        ]
    }
}
