# Contributing

## Getting Started

If you want to start contributing to this project here are a few steps
to get up and running as quickly as possible!

### 1. Clone the Repo

If you haven't already, clone the repo to your development machine:

```
git clone https://github.com/ontologyportal/sigma-rs
```

### 2. Install the Rust Toolchain

This project is written primarily in the Rust programming language. You
can install the full toolchain using the following command (works on 
Windows too!)

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### 3. Install the Node Package Manager (NPM)

The web front end and WASM module use NPM for its dependency management.
To install it, you need to install the Node.js runtime framework which
comes with NPM bundled with it. Instructions for your OS can be found 
here: [Node.js Download Instructions](https://nodejs.org/en/download).

Node 24+ and npm 11+ are expected. This repository is an npm workspace as
well as a Cargo workspace, so install its JavaScript dependencies once from
the repository root:

```
npm install
```

That links `packages/sigmakee`, `packages/web`, `packages/vampire`, and
`packages/language` together, and installs the pinned `binaryen` used to
size-optimize the `.wasm`. You do not need a system `wasm-opt`.

### 4. Install the Git Commit Hooks

Git hooks are script run when you try to commit to the project on your
local machine. While not 100% necessary to install them, they will 
save you some headache when you try to contribute and find yourself 
blocked by the various rules we've created to keep the project 
organization and management smooth.

If you are running Git 2.9.0+, you can use this command to install
the project's Git hooks to your Git instance (local to the project,
this works on both Windows and UNIX):

```
git config core.hooksPath .githooks
```

Otherwise, there is an install script in the .githooks folder (UNIX only):

```
./.githooks/install.sh
```

### 5. Build the project for the first time

To build the project the first time, simply run:

```
cargo build
```

This should produce a binary in the root of the project in the `target/` folder
(note that this folder is ignored by git so you won't accidentally commit a 
build binary).

To open the web application on a development server:

```
SKIP_VAMPIRE=1 npm run web
```

That builds the wasm package and serves the demo at http://localhost:8080/.
Drop `SKIP_VAMPIRE=1` only if you have the Emscripten SDK installed and want
the optional in-browser Vampire backend -- it is a multi-minute build.

To build the publishable npm package on its own:

```
npm run build --workspace sigmakee
```

## Project Guidelines

You may not push directly to the `main` branch. Instead, create a feature branch
then create a PR into `main`. The PR will run a couple of checks before it allows
you to merge it. Most of these checks are run on your local machine via the 
previously installed Git hooks so you should have some degree of confidence that
your commit should pass all the PR merge checks.

The following are some guidelines for submission to the repo:

### Conventional Commits

To enhance our development workflow, enable automated changelog
generation, and pave the way for Continuous Delivery, we've adopted
the [Conventional Commits standard](https://www.conventionalcommits.org/en/v1.0.0/)
for all commit messages.

Going forward, all commits to this repository **MUST** adhere to the
Conventional Commits standard. Commits not adhering to this standard
will cause the CI build to fail. PRs will not be merged if they include
non-conventional commits.

### Unit Tests and Code Linting

All code (at least all the RUST code) must pass unit testing and code/
format linting tests. They can be run easily on your system (they are
also run as a part of the commit hooks):

```
# Run the unit test
cargo test
# Run the linter
cargo clippy
```

No failures are a requirement for submission. Additionally the code must 
compile for the Rust `release` target with zero warnings or errors. You
can simulate that with:

```
cargo build --release
```

### Simulating the GitHub Actions locally

Workflow changes are easy to get wrong and slow to iterate on through
push-and-see. Three tools let you exercise them on your own machine before
opening a PR.

#### Docker

`act` runs each job in a container, so a container runtime has to be running
first. Install Docker Desktop (macOS/Windows) or Docker Engine (Linux) from
[docker.com](https://docs.docker.com/get-started/get-docker/), then start it
and confirm the daemon is up:

```
docker info
```

#### act -- run the workflows

[`act`](https://github.com/nektos/act) executes `.github/workflows/` locally.

```
# macOS
brew install act
# Linux / WSL
curl -sSfL https://raw.githubusercontent.com/nektos/act/master/install.sh | bash
```

Start with a dry run, which validates the job graph and every `if:`
expression without executing anything:

```
act --dryrun -W .github/workflows/regression-pages.yml push
act --dryrun -W .github/workflows/release-version.yml workflow_dispatch
```

To actually execute a job, pin the runner image so `act` does not prompt for
an image size on first use:

```
act -W .github/workflows/regression-pages.yml -j regression push \
    -P ubuntu-latest=catthehacker/ubuntu:act-latest
```

**Read this before running a job for real.** `act` runs against your working
tree and inherits your credentials, and some steps in these workflows have
real side effects:

- `release-version.yml`'s `bump` job ends in `git push origin main` and
  `git tag` -- running it can push to the real repository.
- Its `npm-publish` job ends in a real `npm publish`.
- `regression-pages.yml` deploys to GitHub Pages and Cloudflare Pages, though
  both are skipped when the corresponding secrets are absent.

Use `--dryrun` for those, or copy the steps you care about into a scratch
workflow with the publish/push steps removed (`npm publish --dry-run` is a
safe stand-in). Note also that a full run compiles the wasm crate from
scratch, which fetches the workspace's git submodules and takes several
minutes.

#### wrangler -- check the Cloudflare Pages artifacts

`wrangler pages deploy` has **no** `--dry-run` (that flag is Workers-only),
and any real deploy needs the project's API token. What you *can* do without
credentials is serve the built site through the actual Pages runtime, which
applies `_headers` and `_redirects` exactly as production does:

```
npm run build --workspace @sigma/web
npx wrangler pages dev packages/web/dist --port 8097
```

Then verify the response headers the in-browser Vampire backend depends on:

```
curl -sI http://127.0.0.1:8097/ | grep -i cross-origin
```

You should see `cross-origin-opener-policy: same-origin` and
`cross-origin-embedder-policy: require-corp`. Wrangler also reports how many
`_headers` and `_redirects` rules it accepted, which is the quickest way to
catch a rule that Cloudflare silently ignores.

### Feature brances and contribution size

Do as we say and not as we do. Please do not submit PRs for large features.
Please keep each branch atomic and reference issues with their numbers
where appropriate for your bug fixes features. Bug fix PRs will be looked 
at first.

### LLM Agentic Coding

LLM powered agentic coding/assistance (e.g. Codex, Claude Code, Copilot)
are allowed. However please adhere to the following guidelines:

- Be transparent about AI use. Mark features entirely written by AI using
the [AI] tag in your commit message at the end of each commit addition
description
- If using agentic coders, use the included AGENTS.md/CLAUDE.md to 
instruct it in some lessons learned important to this repo and to 
help avoid slop code

**NO SLOP**: Any PR that is supected of using LLM contributions without
correct attribution + adherance to the agentic coding guidelines will be
rejected. Slop code PRs (large PRs with major feature changes that are 
obviously AI generated) with be REMOVED. All code reviews are conducted by
warm blooded humans!