# Windows Setup — next-gen-editor

Toolchain install steps for a fresh Windows machine.

## 1. Rust (rustup + 1.95.0)

```powershell
winget install Rustlang.Rustup
```

Reopen PowerShell, verify:

```powershell
rustup --version
```

In the repo root, the `rust-toolchain.toml` pin auto-installs Rust 1.95.0 on first `cargo` run.

## 2. Targets + components

```powershell
cd C:\development\Logatta\next-gen-editor
rustup target add wasm32-unknown-unknown
rustup component add clippy rustfmt rust-src
```

## 3. MSVC Build Tools (provides `link.exe`)

Rust on Windows defaults to the MSVC toolchain, which requires Visual Studio Build Tools for the linker. Without it, `cargo install` fails with:

```
error: linker `link.exe` not found
```

Open **PowerShell as Administrator** and run on a single line:

```powershell
winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.Windows11SDK.22621 --includeRecommended"
```

If the paste keeps line-wrapping into `>>` prompts, press `Ctrl+C` and either:

- Save the line into a `.ps1` file and run it, or
- Install the bootstrapper without override:
  ```powershell
  winget install Microsoft.VisualStudio.2022.BuildTools
  ```
  Then open **Visual Studio Installer** GUI → Modify → check **Desktop development with C++** → Install.

Download size: ~3–6 GB.

## 4. wasm-pack

After Build Tools finishes, reopen PowerShell:

```powershell
cargo install wasm-pack
wasm-pack --version
```

## 5. Node + pnpm

Need Node >= 22.

```powershell
winget install OpenJS.NodeJS.LTS
npm install -g pnpm
```

## 6. Verify

```powershell
cd C:\development\Logatta\next-gen-editor
cargo --version
wasm-pack --version
node --version
pnpm --version
```

All four should print versions.

## Alt: skip MSVC, use GNU toolchain

Not recommended (the repo's CI gates assume MSVC), but works for local builds:

```powershell
rustup toolchain install 1.95.0-x86_64-pc-windows-gnu
rustup default 1.95.0-x86_64-pc-windows-gnu
```
