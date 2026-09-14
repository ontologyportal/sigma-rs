/**
 * Node-side Vampire bridge for the `sigmakee/sdk` facade: a `spawnSync` of the
 * `vampire` binary behind the engine's `__sigmaRunVampireSync` global.
 */
import type { VampireBridge } from "./sdk";

/** A bridge running the `vampire` binary at `vampirePath` per problem. */
export function vampireBridge(opts?: { vampirePath?: string }): VampireBridge;
/** Install `bridge` as the global the engine's Vampire runner calls. */
export function installVampireBridge(bridge: VampireBridge): void;
