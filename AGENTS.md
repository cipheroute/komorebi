# Instructions for AI coding agents

You are setting up a personal fork of komorebi (a tiling window manager for Windows) with
Hyprland-like animations. Read [README.md](./README.md) first. The fork is published as is and
was tested on one machine only, so expect to adapt things; the user you work for owns that.

Do not distribute any binaries you build: the [license](./LICENSE.md) only allows
distributing source code.

## 1. Requirements

- Windows 10/11 (developed on Windows 11).
- Upstream komorebi **v0.1.41** installed, eg. `winget install LGUG2Z.komorebi --version 0.1.41`
  (installs to `C:\Program Files\komorebi\bin`). The fork replaces `komorebi.exe` and
  `komorebic.exe` from that install; the other binaries (`komorebi-bar`, etc.) stay upstream.
- Rust stable via rustup with the `x86_64-pc-windows-msvc` target, and Visual Studio Build
  Tools with the "Desktop development with C++" workload (MSVC linker and Windows SDK).

## 2. Build

```powershell
git clone https://github.com/<owner>/komorebi
cd komorebi
git checkout first-simple
cargo build --release -p komorebi -p komorebic -j 8
```

A full build takes several minutes. Limit jobs (`-j`) on weaker machines: building on every
core can make the desktop unresponsive. Outputs: `target\release\komorebi.exe` and
`target\release\komorebic.exe`.

## 3. Install

Writing to `C:\Program Files` needs an elevated shell (UAC). Keep the originals:

```powershell
$bin = 'C:\Program Files\komorebi\bin'
komorebic stop
Copy-Item "$bin\komorebi.exe"  "$bin\komorebi.exe.original-0.1.41"  -ErrorAction SilentlyContinue
Copy-Item "$bin\komorebic.exe" "$bin\komorebic.exe.original-0.1.41" -ErrorAction SilentlyContinue
Copy-Item .\target\release\komorebi.exe  "$bin\komorebi.exe"  -Force
Copy-Item .\target\release\komorebic.exe "$bin\komorebic.exe" -Force
komorebic start
```

Status bars subscribed to komorebi (yasb, komorebi-bar, zebar) usually need a restart after
komorebi restarts. Restore the `.original-0.1.41` files to roll back.

## 4. Configure

Add the `animation` section from README.md to the user's `komorebi.json` (usually
`%USERPROFILE%\komorebi.json`; `komorebic configuration` prints the path) and merge it with
what is there rather than replacing their config. Then `komorebic reload-configuration` or
restart komorebi.

- Write the JSON as UTF-8 **without a BOM**: komorebi silently ignores a file with a BOM
  (PowerShell 5's `Set-Content -Encoding utf8` writes one).
- The `backdrop_profiles` values in README.md were measured on the author's machine. Leave
  them out unless the user has Windows Terminal / Zed with the same look; without a profile
  a default acrylic is used.
- Validate by switching workspaces (eg. `komorebic focus-workspace 1`) and watching
  `%TEMP%\komorebi_plaintext.log.<date>` for errors.

## 5. Optional: faster hotkeys (whkd)

If the user runs whkd and their bindings call `komorebic`, build
[whkd](https://github.com/LGUG2Z/whkd) at tag `v0.2.10` with
`fork-patches/whkd-0.2.10-komorebic-session.patch` applied (`git apply`), and replace
`whkd.exe` the same way as above. Bindings keep the form `komorebic <command>`; whkd routes
them to one `komorebic session` process.

## 6. Optional: yasb patches

Only if the user runs [yasb](https://github.com/amnweb/yasb) **2.0.5** with the komorebi
workspaces and active window widgets. Preferred: build yasb from source at tag `v2.0.5` with
`fork-patches/yasb-2.0.5-fast-workspace-switching.patch`, and its `qt-css-engine` dependency
at commit `2a178cf` with `fork-patches/qt-css-engine-2a178cf-no-full-size-frame.patch`.

Alternative used by the author (a hack, back up `library.zip` first): the installed yasb is
frozen with cx_Freeze, its modules are `.pyc` files in `C:\Program Files\YASB\lib\library.zip`.
Recompile only the patched modules with the same Python minor version as yasb's
`python3XX.dll`, keep the original 16-byte `.pyc` header and `co_filename`, and write a new zip
with those entries replaced:

- `core/widgets/services/komorebi/event_listener.pyc`
- `core/widgets/komorebi/workspaces.pyc`
- `core/widgets/yasb/active_window.pyc`
- `qt_css_engine/engine.pyc`

Check first that the unpatched sources compile to the same bytecode as the installed `.pyc`
files, otherwise the versions don't match and the patch must not be applied this way.
