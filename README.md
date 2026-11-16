# Spotter

A fast file search for Windows that skips the folders you never want results from: dependency stores
(`node_modules`, `site-packages`), caches, version-control data, build output, and Windows' own
system-managed folders (`WinSxS`, `Installer`, `DriverStore` ...).

- **Search** by file name (`report`, or wildcards `*.rs;*.toml`), text inside files, size and modified date
  (`2026-08-23`, or relative: `7d`, `3m`, `today`). **Aa** makes a field case-sensitive.
- **Look in** one or more folders separated by `;`. The drop-down offers **Local drives**, which is the default:
  every fixed drive except network drives and cloud drives such as Google Drive.
- **Results**: click a column header to sort; double-click or Enter opens; right-click to show in Explorer
  or copy paths; Ctrl+C copies the selected path. The **Hits** tab shows the matching lines of the selected file.
- **Summary** shows what was checked and which skip rules fired and how often, so you can always see why
  something was not found. Folders this account may not open (mostly system and vendor folders that need
  administrator) are listed under a collapsed **No access** heading; only real failures appear as errors.

## What is never touched

- **Online-only cloud files and folders** (OneDrive, Dropbox, Google Drive placeholders) are matched by name but
  never opened or listed, so a search never makes the sync client download anything.
- **Symlinks and junctions** are not followed (no loops through pnpm stores and the like).
- **Network drives** are not probed unless you type one into Look in.
- Text search skips binary files and files over the size limit on the Options tab (50 MB by default).
  UTF-8 and UTF-16 (with byte order mark) files are read.

## Skip list

Edit it on the **Options** tab (untick *Skip ignored folders* to search everything). One rule per line:

```
node_modules                 every folder with this name (* and ? allowed)
target if ../Cargo.toml      only when the parent folder has a matching file
venv if pyvenv.cfg           only when the folder itself contains a matching file
Windows/WinSxS               a rule with / matches the end of the folder's path
C:/Data/Big                  ... or the whole path, when it has a drive
```

A skipped folder can still be found by name; only its contents are not searched.
Settings and the skip list are saved in `%APPDATA%\Spotter\settings.toml`.

## Command line

```
spotter.exe D:\Github report                open the window and search straight away
spotter-cli D:\Github report                print results in the console
spotter-cli local *.pdf --after 30d         every local drive, modified in the last 30 days
spotter-cli D:\src --text TODO --files      files containing TODO
spotter-cli D:\Github report --no-skip      include ignored folders
```

## Build

```
cargo build --release      # target\release\spotter.exe (GUI) and spotter-cli.exe
cargo test --lib
```
