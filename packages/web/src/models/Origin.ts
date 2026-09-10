import { SUMO } from "../constants";

/** The three places a constituent's text can come from -- also the string
 *  the worker/localStorage/upstream-fetch code branches on, so every
 *  `Origin` subclass carries the matching literal as `kind`. */
export type OriginKind = "sumo" | "file" | "url";

/** The persisted shape of an `Origin` (see `serializeOrigin` / `parseOrigin`). */
export type OriginJson =
  | { kind: "sumo"; owner: string; repo: string; branch: string }
  | { kind: "file" }
  | { kind: "url"; url: string };

export abstract class Origin {
  abstract readonly kind: OriginKind;
}

/** A file tracked in a git-hosted repository (GitHub only for now). The
 *  upstream `ontologyportal/sumo` repo is the default; its files are named by
 *  bare repo path, another repo's files by `owner/repo/path`. */
export class GitOrigin extends Origin {
  readonly kind = "sumo" as const;
  service: "github" | "gitlab";
  owner: string;
  repo: string;
  branch: string;

  constructor(
    service: "github" | "gitlab",
    owner: string,
    repo: string,
    branch: string,
  ) {
    super();
    this.service = service;
    this.owner = owner;
    this.repo = repo;
    this.branch = branch;
  }

  static default(): GitOrigin {
    return new GitOrigin("github", SUMO.owner, SUMO.repo, SUMO.branch);
  }

  /** `github:owner/repo@branch` -- the library's repo key. */
  get id(): string {
    return `${this.service}:${this.owner}/${this.repo}@${this.branch}`;
  }

  get isDefault(): boolean {
    return (
      this.owner === SUMO.owner &&
      this.repo === SUMO.repo &&
      this.branch === SUMO.branch
    );
  }

  get label(): string {
    return `${this.owner}/${this.repo}@${this.branch}`;
  }

  rawUrl(path: string): string {
    return `https://raw.githubusercontent.com/${this.owner}/${this.repo}/${this.branch}/${path}`;
  }

  /** The KB name of a repo path: bare for the default repo, prefixed with
   *  `owner/repo/` for any other so names never collide across repos. */
  nameFor(path: string): string {
    return this.isDefault ? path : `${this.owner}/${this.repo}/${path}`;
  }

  /** Inverse of `nameFor`. */
  pathOf(name: string): string {
    const prefix = `${this.owner}/${this.repo}/`;
    return !this.isDefault && name.startsWith(prefix)
      ? name.slice(prefix.length)
      : name;
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

/** An arbitrary URL. An empty `url` means the constituent's name is the
 *  URL (how URL constituents were named before the library existed). */
export class RemoteOrigin extends Origin {
  readonly kind = "url" as const;
  url: string;
  accessed: Date;

  constructor(url = "", date?: Date) {
    super();
    this.url = url;
    this.accessed = date || new Date();
  }
}

/** The inverse of `.kind` -- reconstruct a fresh `Origin` instance from the
 *  discriminant alone (what the saved-tests list carries). */
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

export function serializeOrigin(o: Origin): OriginJson {
  switch (o.kind) {
    case "sumo": {
      const g = o as GitOrigin;
      return { kind: "sumo", owner: g.owner, repo: g.repo, branch: g.branch };
    }
    case "file":
      return { kind: "file" };
    case "url":
      return { kind: "url", url: (o as RemoteOrigin).url };
  }
}

/** Rebuild an `Origin` from its persisted form. A bare kind string is the
 *  pre-library saved format and maps to `originForKind`; a `url` origin
 *  without a URL takes `name` as the URL. */
export function parseOrigin(
  json: OriginJson | OriginKind | null | undefined,
  name = "",
): Origin {
  if (typeof json === "string") {
    const o = originForKind(json);
    if (o.kind === "url") (o as RemoteOrigin).url = name;
    return o;
  }
  switch (json?.kind) {
    case "sumo":
      return new GitOrigin(
        "github",
        json.owner || SUMO.owner,
        json.repo || SUMO.repo,
        json.branch || SUMO.branch,
      );
    case "url":
      return new RemoteOrigin(json.url || name);
    case "file":
      return new LocalOrigin();
    default:
      return GitOrigin.default();
  }
}

/** The library tables' Source column text: `GitHub` for the upstream repo,
 *  `owner/repo@branch` for any other, `Local`, or `URL`. */
export function sourceLabel(o: Origin): string {
  switch (o.kind) {
    case "sumo":
      return (o as GitOrigin).isDefault ? "GitHub" : (o as GitOrigin).label;
    case "file":
      return "Local";
    case "url":
      return "URL";
  }
}

/** A short stable identity for an origin, independent of any file: the git
 *  id for repos, `file` for local uploads, `url:<url>` for URLs. Used with a
 *  name to fingerprint the loaded constituent set. */
export function originId(json: OriginJson | Origin): string {
  switch (json.kind) {
    case "sumo": {
      const g = json as { owner: string; repo: string; branch: string };
      return `github:${g.owner}/${g.repo}@${g.branch}`;
    }
    case "file":
      return "file";
    case "url":
      return `url:${(json as { url: string }).url}`;
  }
}
