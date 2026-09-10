import { SUMO } from "../constants";

/** The three places a constituent's text can come from -- also the string
 *  the worker/localStorage/upstream-fetch code branches on, so every
 *  `Origin` subclass carries the matching literal as `kind`. */
export type OriginKind = "sumo" | "file" | "url";

export abstract class Origin {
  abstract readonly kind: OriginKind;
}

/** A file tracked in the upstream `ontologyportal/sumo` repo. */
export class GitOrigin extends Origin {
  readonly kind = "sumo" as const;
  service: "github" | "gitlab";
  owner: string;
  repo: string;
  branch: string;

  constructor(service: "github" | "gitlab", owner: string, repo: string, branch: string) {
    super();
    this.service = service;
    this.owner = owner;
    this.repo = repo;
    this.branch = branch;
  }

  static default(): GitOrigin {
    return new GitOrigin("github", SUMO.owner, SUMO.repo, SUMO.branch);
  }
}

/** An OPFS-backed local upload. */
export class LocalOrigin extends Origin {
  readonly kind = "file" as const;
  added: Date;

  constructor(date?: Date) {
    super();
    this.added = date || new Date();
  }
}

/** An arbitrary URL. */
export class RemoteOrigin extends Origin {
  readonly kind = "url" as const;
  accessed: Date;

  constructor(date?: Date) {
    super();
    this.accessed = date || new Date();
  }
}

/** The inverse of `.kind` -- reconstruct a fresh `Origin` instance from the
 *  discriminant alone (what localStorage / the saved-constituent list carries). */
export function originForKind(kind: OriginKind): Origin {
  switch (kind) {
    case "sumo":
      return GitOrigin.default();
    case "file":
      return new LocalOrigin();
    case "url":
      return new RemoteOrigin();
  }
}
