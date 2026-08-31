# chef

chef is a command-line tool to install, update and remove mods and plugins for GTA games. It detects current game, backs up replaced files, and quickly switches versions with `pkg@version`. Currently it is Windows-only.

## Install

PowerShell (Windows):

```powershell
irm https://raw.githubusercontent.com/x87/chef/master/install.ps1 | iex
```

Git Bash (or any Unix shell):

```sh
curl -fsSL https://raw.githubusercontent.com/x87/chef/master/install.sh | bash
```

or download the binary from [the releases page](https://github.com/x87/chef/releases) and add it to your PATH.

## Commands

Once installed, run the following commands in your favorite terminal in the game folder (or use `--dir <folder>`):

| Command               | What it does                             |
| --------------------- | ---------------------------------------- |
| `chef menu`           | list available packages for your game    |
| `chef which [<pkg>]`  | show what is installed in current folder |
| `chef add <pkg>`      | install a mod                            |
| `chef remove <pkg>`   | uninstall a mod (restores backups)       |
| `chef update [<pkg>]` | update installed mods                    |
| `chef upgrade`        | update chef itself                       |
| `chef help`           | all commands and options                 |

## Options

| Flag             | Applies to           | What it does                                 |
| ---------------- | -------------------- | -------------------------------------------- |
| `--dir <folder>` | all except `upgrade` | game folder instead of the current directory |
| `--json`         | all                  | machine-readable output and errors           |
| `--dry-run`      | `add`, `update`      | show what would happen, change nothing       |
| `--refresh`      | `menu`               | re-download the catalog                      |
| `--check`        | `upgrade`            | report whether an update exists              |

## Examples

```
chef add cleo
chef add cleo@5                       newest stable 5.x
chef add cleo@4.4.4                   exact version
chef add cleo@latest                  newest release, pre-releases included
chef add cleo@preview                 newest pre-release only
chef add cleo@latest silentpatch sal  several packages at once
chef remove cleo sal
chef update --dry-run
chef upgrade --check
```

Names match loosely (`cleo-red` finds `cleo-redux`). If a name matches several packages, chef asks which one. Unknown versions are rejected:

```
$ chef add cleo@6
error: version '6' does not match any tracked release (available majors: 4, 5)
```

## Menu

```
$ chef menu
TITLE                AVAILABLE            INSTALLED
CLEO                 5.4.0, 4.4.4         -
VC.CLEO              2.2.0                -
CLEO Redux           1.5.0                -
Silent's ASI Loader  1.5.0 (preview), 1.3.0 -
Universal ASI Loader 9.7.4                -
```

Pre-releases are marked `(preview)` and only selected via `@preview` or `@latest`.

## Which

`chef which` lists installed mods in two groups: chef-managed and user-installed (user mods are found by checksum):

```
$ chef which

game dir: D:\Games\San Andreas
game: GTA San Andreas

PACKAGES INSTALLED BY CHEF
WidescreenFix          <no version>

PACKAGES INSTALLED BY USER
CLEO                 multiple  run 'chef which cleo5' for more details
CLEO Redux           unknown   run 'chef which cleo-redux' for more details
Silent's ASI Loader  1.5.0
```

`chef which <pkg>` lists the selected package's files:

```
$ chef which sal
scripts\global.ini  unknown
vorbisFile.dll      unknown
vorbisHooked.dll    1.5.0
```

`unknown` means the file is present but its checksum does not match any known version;

## Dry runs

`chef add --dry-run` prints the plan file by file:

```
Silent's ASI Loader 1.3.0
  backup:
    scripts/global.ini  user file backed up, restored on 'chef remove'
    vorbisFile.dll      user file backed up, restored on 'chef remove'
  keep:
    vorbisHooked.dll    already installed
```

`chef update --dry-run` prints the same plan for all installed packages, and `chef remove --dry-run` prints the files that would be restored from backup.

## Notes

- chef backs up everything before changing it and restores on `remove`
- State, cache and backups live in `%LOCALAPPDATA%\Chef`; `history.log` there keeps track of recent messages and executed commands for debugging
