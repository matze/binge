# binge – fetch and manage GitHub release binaries

[![CI](https://github.com/matze/binge/actions/workflows/ci.yml/badge.svg)](https://github.com/matze/binge/actions/workflows/ci.yml)

`binge` is a command-line application written in Rust that simplifies the
process of fetching, installing, updating, and managing binary release artifacts
directly from GitHub repositories. It is designed to be a convenient tool for
developers and users who rely on tools distributed as pre-built binaries on
GitHub Releases.

Similar in concept to tools like [eget](https://github.com/zyedidia/eget) and
[cargo-binstall](https://github.com/cargo-bins/cargo-binstall), `binge`
distinguishes itself by:

* Keeping track of installed binaries, allowing for easy updates.
* Not being restricted to projects written in Rust (unlike `cargo-binstall`).
* Offering a simple and focused command-line interface.

`binge` works by inspecting the releases of a given GitHub repository,
identifying a suitable binary artifact for your system's architecture,
downloading it, and placing it in a designated installation directory. It
maintains a manifest file to keep track of what has been installed and where.

> [!CAUTION]
> Use this tool at your own risk. It installs binaries from third-party GitHub
> repositories, which could potentially be malicious and harm your system.
> Always ensure you trust the source of the binaries you install.

## Installation

As of now you have to run `binge` from source or install from a checkout with `cargo install --path .`

## Usage

`binge` provides several subcommands to manage your installed binaries:

### `binge install <owner/repo>...`

Installs one or more binaries from the specified GitHub repositories. The format
for specifying a repository is `<owner>/<repo>`.

```bash
binge install sharkdp/fd BurntSushi/ripgrep
```

This command will find the latest release for `sharkdp/fd` and
`BurntSushi/ripgrep`, download the appropriate binary artifact for your system,
and install it.

In some cases, the downloaded binary might have a different name than desired,
or you might want to avoid naming conflicts. You can specify a custom name for
the installed binary by adding a colon `:` followed by the desired name after
the repository path:

```bash
binge install idursun/jjui:jjui
```

This will download the binary from `idursun/jjui` that contains an
architecture-specific suffix but install it as `jjui` in your installation
directory.

### `binge uninstall <owner/repo>...`

Uninstalls one or more binaries that were previously installed by `binge`.
Specify the binaries using the `<owner>/<repo>` format.

```bash
binge uninstall sharkdp/fd BurntSushi/ripgrep
```

### `binge update`

Checks all currently installed binaries for new releases on GitHub and updates
them if a newer version is available.

```bash
binge update
```

### `binge rename <owner/repo>`

Renames a binary that was previously installed by `binge`. Specify the binary
and the new name using the `<owner>/<repo>:<custom>` format.

```bash
binge rename idursun/jjui:jjui
```

### `binge list`

Lists all binaries currently installed by `binge`.

```bash
binge list
```

By default, this command prints the repository and installed version for each binary:

```
sharkdp/fd 8.7.1
BurntSushi/ripgrep 13.0.0
idursun/jjui 0.6.0
```

You can get a list of installed binaries formatted in a way suitable for the
`binge install` command using the `install` format:

```bash
binge list install
```

This will output something like:

```
sharkdp/fd BurntSushi/ripgrep idursun/jjui:jjui
```

This output can be useful for reinstalling the same set of binaries on another
machine or after a system reinstallation.

### `binge completion <shell>`

Generates shell completion scripts for your preferred shell. Replace `<shell>`
with your shell's name (e.g., `bash`, `zsh`, `fish`, `powershell`, `elvish`).

```bash
binge completion bash
```

Follow the instructions provided by the output to integrate completion with your
shell.


## Configuration

### GitHub Personal Access Token

By default, `binge`'s interactions with the GitHub API may be subject to rate
limits. To avoid this, it is highly recommended to set the `GITHUB_TOKEN`
environment variable to a GitHub personal access token.

For enhanced security, consider creating a fine-grained token with minimal
necessary permissions (e.g., read access to public repositories).

```bash
export GITHUB_TOKEN="your_token_here"
```

Add this line to your shell's profile file (e.g., `~/.bashrc`, `~/.zshrc`) to
make it persistent.

## Platform Support

Currently, `binge` only supports **Linux**.

## License

This project is licensed under the [MIT](./LICENSE) license.
