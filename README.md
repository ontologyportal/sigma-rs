<div align="center">
  <img src="./logo.png" alt="SUPr Logo" width="100" style="background:#ddd;padding:10px;border-radius:10px">
  <div style="font-weight:bold;font-size:24px">SigmaKEE-rs</div>
</div>
<br>

A parser, validator, and theorem-prover interface for the [SUO-KIF](https://www.ontologyportal.org/suo-kif.pdf) / [SUMO](https://www.ontologyportal.org/) knowledge representation language.

KIF files are parsed once and committed to an [LMDB](https://www.symas.com/lmdb) database. Interact with
the knowledge base with a configurable (and extendable) automated theorem prover backend. SigmaKEE-rs 
currently supports the following automated theorem provers (ATPs) for automated reasoning against SUMO:

- [Vampire](https://vprover.github.io/) - both embedded as an API and via subprocess invocation
- [E](https://github.com/eprover/eprover) - via subprocess invocation
- SUPr (SUMO Prover) - a prover built and optimized specifically for reasoning over SUMO!

[Check out the current test results against the current version of SUMO (run nightly)](https://ontologyportal.github.io/sigma-rs/)

## Table of Contents
- [Install](#install)
  * [CLI](#cli-installation)
    * [Official Release](#from-official-release-channel)
      - [UNIX](#unix-macos-intelarm64--linux-arm64--linux-amd64)
      - [Windows](#windows)
    * [Building from Source](#build-from-source)
  * [Web App](#web-application-installation)
    * [Official Release](#sigmakeedev)
    * [Local Server](#local-server)
  * [VSCode Extension](#vscode-extension-installation)
    * [Official Release](#vscode-extension-marketplace)
    * [Development Build](#install-extension-from-source)
- [Workspace Layout](#workspace-layout)
- [User Guides](#user-guides)
- [API Documentation](#developer-api-documentation)
- [Bug Reports](#bug-reports)
- [Contributing](#contributing)

## Install

There are multiple user applications provided by this repository. The **Command Line Interface (CLI)** 
provides the full SigmaKEE functionality via a commandline and is best suited for CI/CD pipelines, agents, or 
those who just prefer a command line experience! The **Web application** is a pretty, self-contained web GUI 
which provides all the same features of the CLI but in a pretty graphical interface. It also comes bundled 
with an in-browser IDE ([Microsoft's Monaco Editor](https://microsoft.github.io/monaco-editor/)) with a small 
feature subset including intellisense, validation hints, and autocomplete. Finally, the **VSCode extension** 
is a direct integration with [Microsoft's VSCode IDE](https://code.visualstudio.com/). This provides the full 
SigmaKEE experience centered around the developer's SUO-KIF writing experience. The following sections provide details for installing each front end, both from official release channels and via source.

### CLI Installation

#### From Official Release Channel

Using the official GitHub release channel is best for those who **DO NOT** wish to 
customize their installation of `sigmakee-rs`. After installing via this channel, you 
will not have to rerun this command to get updates (hopefully).

##### UNIX (macOS Intel/ARM64 + Linux arm64 + Linux amd64)

Copy and paste the following in your terminal.

```bash
curl -fsSL https://raw.githubusercontent.com/ontologyportal/sigma-rs/main/install.sh | bash
```

##### Windows

Copy and paste the following in your PowerShell terminal.

```powershell
irm https://raw.githubusercontent.com/ontologyportal/sigma-rs/main/install.ps1 | iex
```

**Warning: The Windows build currently does NOT include the embedded Vampire prover build due to
preexisting compilation errors. This is future work**

#### Build from source

Only use this method if you intend on modifying your build. You will be responsible for maintaining provenance over your updates.

To compile from source, first install [Rust](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then clone this repository:

```bash

git clone https://github.com/ontologyportal/sigma-rs && cd sigma-rs
```

Compile everything (Cargo fetches the Vampire C++ bindings directly from their git repo as an 
ordinary dependency):

```bash
cargo build --release --bin sumo
```

For **Windows**, you have to exclude the `integrated-prover`
feature:

```powershell
cargo build --release --bin sumo --no-default-features --features ask,parallel,alloc-mi
```

The executable is located in `target/release/sumo`. You can link it to your system PATH using (UNIX):

```bash
sudo ln -s $PWD/target/release/sumo /usr/local/bin/sumo
```

For Windows, you have to manually add it to your PATH or set up a PowerShell alias.

### Web Application Installation

#### sigmakee.dev

Simple! Open your favorite web browser (modern web browsers only, sorry Internet Explorer enthusiasts). Then navigate to our official site: [sigmakee.dev](https://sigmakee.dev). No account is required if you only plan on navigating the knowledge base. The web application uses GitHub as its "backend" so you can log in via GitHub OAuth to get the full feature set.

#### Local Server

If you wish to run the web application directly from source (e.g. for development), you will need both Rust and Node.js installed on you system. For UNIX (macOS, Linux, WSL), Rust and Node are installed:

```bash
# Install Rust to compile the Web Assembly library
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# Install Node Version Manager (NVM)
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.7/install.sh | bash
# Add NVM to your PATh
\. "$HOME/.nvm/nvm.sh"
# Install latest Node version
nvm install latest
```

For Windows, you can download both dependencies via the [Chocolatey](https://chocolatey.org/) 
package manager:

```powershell
# Download and install Chocolatey package manager:
powershell -c "irm https://community.chocolatey.org/install.ps1|iex"
# Install Rust to compile the Web Assembly Library
choco install rustup.install
# Download and install Node.js:
choco install nodejs --version="24.19.0"
```

Additionally, if you wish to install the web application with external theorem provers (Vampire) 
you'll have to install [Emscripten](https://emscripten.org/) (a web assembly linker for C/C++), 
Gawk, and CMake. Installation varies by specific platform. For Emscripten, you can follow their 
directions [here](https://emscripten.org/docs/getting_started/downloads.html). `cmake` and `gawk` 
are generally available via your platform's package manager (e.g. `sudo apt install cmake gawk`).

Once installed, clone the repo:

```bash
git clone https://github.com/ontologyportal/sigma-rs
```

Install node dependencies:

```bash
npm install
```

To run the sever in development mode (e.g. it will rebuild the sources when you change a file):

```bash
npm run web
```

**Important**: this creates a dev server which runs the backend API calls. To utilize the calls, 
you need to provide the backend server with a GitHub OAuth configuration. If you do not need GitHub
API access for your dev, skip this step (e.g. you do not need to test the PR features). Otherwise
follow these instructions to make your own test OAuth application:

1. Open a web browser and navigate to: 
 [https://github.com/settings/developers](https://github.com/settings/developers) (you will need to 
 log into GitHub)
2. Click "New OAuth App"
3. Create a dummy **Application Name** (e.g. "Test SigmaKEE") 
4. Enter `https://localhost:8788` as the **Homepage URL**
5. Enter `https://localhost:8788/api/github-auth-callback` as the **Authorization callback URLs** entry
6. Uncheck **Expire user access tokens**
7. Save the **Client ID** code on the next page. You will use it as the `GITHUB_OAUTH_CLIENT_ID`.
8. Under "Client secrets" click **Generate a new client secret**. It will generate a code. Copy 
 the code (you can only copy it once, so make sure to do this). You will use this for 
 `GITHUB_OAUTH_CLIENT_SECRET`.
9. Create the file `packages/web/.dev.vars`
10. Populate that file with the following contents (replacing the X's with the values from the previous steps)

```
GITHUB_OAUTH_CLIENT_ID="XXXXXXXXXXXXXXXXXXXX"
GITHUB_OAUTH_CLIENT_SECRET="XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
```

11. Rerun `npm run web`. It will now properly make OAuth API calls for you.

**Optionally,** use environment variables to customize your local web app build:

| Variable | Effect |
|---|---|
| `NO_REBUILD=1` | Skip the wasm rebuild and serve whatever is already built |
| `SKIP_VAMPIRE=1` | Skip the Vampire backend build (a multi-minute Emscripten build) |
| `VAMPIRE_RECLONE=1` | Force a clean Vampire rebuild |

```bash
SKIP_VAMPIRE=1 npm run web    # skip the Emscripten toolchain
```

### VSCode Extension Installation

#### VSCode Extension Marketplace 

The VSCode extension is currenly unpublished. A legacy extension is still available on the VSCode
extension marketplace under the name [SUMO](https://marketplace.visualstudio.com/items?itemName=ontologyportal.sumo)
however this will be superceded by a new extension called SigmaKEE. This section will be updated once
the new extension is released and published.

#### Install Extension from Source

You can install the extension locally. To do so, you will need to install the 
[same prerequisites as the web application](#local-server). Then run:

```bash
npm run vscode:compile
```

This will produce a `.vsix` VSCode extension package file: `packages/vscode/sigmakee-vscode-*.*.*.vsix`. 
You can install that manually in VSCode via the extensions window (using the Command runner, 
`> Extensions: Install from VSIX...`).

---

## Workspace layout

This repository is **both a Cargo workspace and an npm workspace**: `crates/`
holds the Rust members, `packages/` holds the JavaScript members. The two meet
at `crates/wasm`, whose compiled output is what the `sigmakee` npm package
publishes and what the demo site runs in the browser. Both the VSCode extension and 
web application rely on a common language server (LSP), the `lsp` crate. Both the `cli`
and `lsp` crates are built on the `sdk` crate which exposes a programmer friendly interface
for the `core` crate.

| Crate | Description |
|---|---|
| `crates/core` (`sigmakee-rs-core`) | Core library for the Sigmakee implementation |
| `crates/sdk` (`sigmakee-rs-sdk`) | SDK which makes software consumption of `sigmakee-rs-core` more intuitive |
| `crates/cli` (`sigmakee`) | Command line interface for SUMO, builds the `sumo` executable |
| `crates/lsp` (`sumo-lsp`) | Persistent language server for IDE integration |
| `crates/wasm` (`sumo-parser-wasm`) | WASM bindings for the browser / Node.js |

| Package | Published? | Description |
|---|---|---|
| `packages/sigmakee` (`sigmakee`) | yes | The publishable wasm package: `crates/wasm` built with `wasm-bindgen`, plus the SDK-shaped `./sdk` facade |
| `packages/web` (`@sigma/web`) | no | The SUMO browser demo site (Vite); consumes `sigmakee` as a workspace dependency |
| `packages/vampire` (`@sigma/vampire`) | no | Emscripten build of the Vampire prover, for the optional in-browser "Vampire (WASM)" backend |
| `packages/language` (`@sigma/language`) | no | Editor-neutral SUO-KIF / TPTP language assets shared by the web app and the VSCode extension |
| `packages/vscode` (`sigmakee-vscode`) | not yet | VSCode integration for SigmaKEE

## User Guides

For details for how to run each front end provided as a part of this repository see the individual 
READMEs for each component:

- [CLI User Guide](./crates/cli/README.md)
- [Web Application User Guide](./packages/web/README.md)
- [VSCode Extension User Guide](./packages/vscode/README.md)

## Developer API Documentation

This repo has two "SDK" packages which expose the primary functionality of SigmaKEE via a programatic
interface:

- **Rust SDK**: The Rust SDK's documentation is viewable via Rust docs. On your local machine, your can 
build the Rust docs using the command: `cargo doc`. Documentation is then available at [./target/doc/sigmakee_rs_sdk/index.html](./target/doc/sigmakee_rs_sdk/index.html). **We are currently working on hosting the Rust Docs via GitHub pages. This is COMING SOON**. 
- [JavaScript SDK Reference](./packages/sigmakee/README.md).

## Bug Reports

To submit a bug report / feature request for any of the applications hosted in this repo, please 
submit a [GitHub issue](https://github.com/ontologyportal/sigma-rs/issues/new?q=is%3Aissue+state%3Aopen) 
with the appropriate label (e.g. `vscode` for issues pertaining to the VSCode extension, `cli` for 
issues pertaining to the CLI, etc).

## Contributing

Please read our [CONTRIBUTING.md](./CONTRIBUTING.md) for information about how to contribute to the project
to include our project's responsible AI use policy.