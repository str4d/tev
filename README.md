# tev: Thermal Expansion Valve

`tev` is a tool for interacting with Steam game files, with a current focus on
Steam backups.

## Installation

On Unix systems:
```bash
$ cargo install tev
```

On Windows systems:
```bash
$ cargo install tev --no-default-features
```

## Usage

### Inspect a Steam file

`tev` supports inspecting several kinds of Steam files:

| Kind                | File extension |
|---------------------|----------------|
| SKU                 | `.sis`         |
| ChunkStore Manifest | `.csm`         |
| ChunkStore Data     | `.csd`         |
| Content Manifest    | `.manifest`    |

```bash
$ tev inspect "path/to/backups/Half-Life Alyx/sku.sis"
SKU: Half-Life: Alyx (Disk 1/1)
Backup: 1
Content type: 3
Apps:
- 546560
Depots:
- 546561, manifest: 6340340699246199351, Size: 53.77 GiB
- 546563, manifest: 8142939566738022382, Size: 652.94 MiB
```

### Verify a Steam backup

`tev` can verify that a Steam backup is internally consistent:

```bash
$ tev backup verify "path/to/backups/Half-Life Alyx/"
Game: Half-Life: Alyx
Verifying depot 546561
Verifying depot 546563
Depot files match SKU!
```

### Mount a Steam backup

On Unix systems with the default-enabled `mount` feature flag, `tev` can mount an existing
Steam backup as a directory:

```bash
$ tev backup mount --manifest-dir path/to/manifests path/to/backups/GameName ./mountpoint
[Ctrl-C to stop]
```

While `tev backup mount` is running, the folder `./mountpoint` will contain a
read-only view of the game's files, with the same folder layout as when the game
is installed.

Reading the contents of a Steam backup requires access to its manifest file,
which is not included in backups. When you install a game that you own, its
manifest file is cached in the `depotcache` folder inside the main Steam install
folder.

## License

Licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
