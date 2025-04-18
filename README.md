# binge – fetch binaries

In a similar vein to [eget][] and [cargo binstall][], binge installs binary
release artifacts from GitHub matching the host architecture. Compared to eget,
binge also keeps track of installed binaries and allows for updates while it is
not restricted to Rust-based projects like cargo-binstall.

[eget]: https://github.com/zyedidia/eget
[cargo binstall]: https://github.com/cargo-bins/cargo-binstall

> [!CAUTION]
> You use this tool at your own risk. It installs binaries that are
> potentially malicious and may harm your system. You have been warned!

## Usage

To install one ore more binaries, look up the GitHub owner and repos and run

```bash
binge install owner/repo ...
```

For example to install `fd` and `rg`, run

```bash
binge install sharkdp/fd BurntSushi/ripgrep
```

Similarly, you can uninstall binaries:

```bash
binge uninstall sharkdp/fd BurntSushi/ripgrep
```

To update all binaries to their latest version use

```bash
binge update
```


## Platform support

As of now, only Linux is supported.


## License

[MIT](./LICENSE)
