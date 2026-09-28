# chef

chef is a command-line tool to install, update and remove mods and plugins for GTA games. It detects current game, backs up replaced files, and quickly switches mod versions.

⚠️ This is an early prototype. It may or may not work as advertised. Use at your own risk.

## Install

Windows (Powershell):

```powershell
irm https://raw.githubusercontent.com/x87/chef/master/install.ps1 | iex
```

Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/x87/chef/master/install.sh | bash
```

or download the binary from [the releases page](https://github.com/x87/chef/releases) and add it to your PATH.

## Commands

Once installed, run the following commands in your favorite terminal in the game folder (or use `--dir <folder>`):

| Command               | What it does                              |
| --------------------- | ----------------------------------------- |
| `chef menu`           | list available packages for your game     |
| `chef which [<pkg>]`  | show what is installed in current folder  |
| `chef add <pkg>`      | install a mod                             |
| `chef remove <pkg>`   | uninstall a mod (replays the undo script) |
| `chef update [<pkg>]` | update installed mods                     |
| `chef upgrade`        | update chef itself                        |
| `chef help`           | all commands and options                  |

## Options

| Flag             | Applies to           | What it does                                 |
| ---------------- | -------------------- | -------------------------------------------- |
| `--dir <folder>` | all except `upgrade` | game folder instead of the current directory |
| `--json`         | all                  | machine-readable output and errors           |
| `--dry-run`      | `add`, `update`      | show what would happen, change nothing       |
| `--refresh`      | `menu`               | re-download the catalog                      |
| `--check`        | `upgrade`            | report whether an update exists              |

## `chef menu`

`chef menu` displays a list of packages available for the game in current directory:

```
$ chef menu
TITLE                AVAILABLE            COMMAND
CLEO                 5.4.0, 4.4.4         chef add cleo
CLEO Redux           1.5.1                chef add cleo-redux
Silent's ASI Loader  1.5.0 (preview), 1.3.0 chef add sal
Universal ASI Loader 9.7.4                chef add ual
SilentPatch          34.1.0, 33.1.0       chef add silentpatch
WidescreenFix        <no version>         chef add widescreenfix
Mod Loader           0.3.10               chef add modloader
Windowed Mode        2.2.0, 2.1.0         chef add windowedmode
SaveLoader           2.7.0                chef add saveloader
CLEO Plus            1.2.0                chef add cleoplus
NewOpcodes           2.5.1                chef add newopcodes
```

Pre-releases are marked `(preview)` and only selected via `@preview` or `@latest`. Use `@stable` to download the latest stable release.

## `chef add`

```
chef add cleo
chef add cleo@5                       newest stable 5.x
chef add cleo@4.4.4                   exact version
chef add cleo@latest                  newest release, pre-releases included
chef add cleo@preview                 newest pre-release only
chef add cleo@latest silentpatch sal  several packages at once
```

Names match exactly against a package's full name or one of its aliases (for example `cleo-redux` or `redux`). Partial names and typos are rejected. If a name matches several packages, chef asks which one. Unknown versions are rejected:

```
$ chef add cleo@6
error: version '6' does not match any tracked release (available majors: 4, 5)
```

## `chef remove`

`chef remove <pkg>` uninstalls the package(s) reverting any modifications:

```
chef remove cleo sal
```

If the version you remove had replaced an older one, that older version comes back:

```
$ chef remove newopcodes@2.5.0
removed NewOpcodes 2.5.0
restored previous installation: NewOpcodes 2.5.1
```

## `chef which`

`chef which` lists installed mods in two groups: chef-managed and user-installed (mods you installed yourself are found by their contents):

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

`unknown` means the file is there, but its contents do not match any version chef knows for that mod.

## `chef --dry-run`

`chef add --dry-run` prints the plan file by file:

```
Silent's ASI Loader 1.3.0
  backup:
    scripts/global.ini  user file backed up, restored on 'chef remove'
    vorbisFile.dll      user file backed up, restored on 'chef remove'
  keep:
    vorbisHooked.dll    already installed
```

`chef update --dry-run` prints the same plan for all installed packages.

## Notes

- chef writes down every file it touches and how to undo each change, so `remove` can put the game folder back exactly the way it was before that install; if the install had replaced an older version, that version comes back and stays listed as installed instead of being left behind as a stray file
- State, cache and backups live in `%LOCALAPPDATA%\Chef`; `history.log` there keeps track of recent messages and executed commands for debugging
